use actix_files as afs;
use actix_web::{rt::spawn, web, App, HttpResponse, HttpServer, Responder};
use ollama_rs::{generation::completion::request::GenerationRequest, Ollama};
use serde::{Deserialize, Serialize};
use serde_json::json;

use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex},
};

const MODEL_NAME: &str = "llama3.2";

#[derive(Clone, Serialize, Deserialize, Default)]
struct Progress {
    status: String,
    iteration: u32,
    max_iteration: u32,
    output: String,
    completed: bool,
}

#[derive(Deserialize)]
struct UserInput {
    user_input: String,
}

async fn home(progress: web::Data<Mutex<Progress>>) -> impl Responder {
    // Reset the progress
    {
        let mut progress_guard = progress.lock().unwrap();
        *progress_guard = Progress::default();
    }

    let index_path = Path::new("templates/index.html");

    if index_path.exists() {
        match fs::read_to_string(index_path) {
            Ok(contents) => HttpResponse::Ok().content_type("text/html").body(contents),
            Err(_) => HttpResponse::InternalServerError().body("Error reading index.html"),
        }
    } else {
        HttpResponse::Ok().content_type("text/html").body(
            r#"
            <h1>Flask App Generator</h1>
            <form method="post">
                <label for="user_input">Describe the Flask app you want to create or modify:</label><br>
                <input type="text" id="user_input" name="user_input" size="50"><br><br>
                <input type="submit" value="Generate/Update Flask App">
            </form>
            "#,
        )
    }
}

// Progress route handler
async fn get_progress(progress: web::Data<Mutex<Progress>>) -> impl Responder {
    let progress = progress.lock().unwrap();
    web::Json(progress.clone())
}

// Main function to run the application
#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let progress = web::Data::new(Mutex::new(Progress::default()));

    HttpServer::new(move || {
        App::new()
            .app_data(progress.clone())
            .service(afs::Files::new("/static", "static").show_files_listing())
            .service(
                web::resource("/")
                    .route(web::get().to(home))
                    .route(web::post().to(handle_post)),
            )
            .service(web::resource("/progress").route(web::get().to(get_progress)))
    })
    .bind("0.0.0.0:8080")?
    .run()
    .await
}

// Function to handle POST requests
async fn handle_post(
    form: web::Form<UserInput>,
    progress: web::Data<Mutex<Progress>>,
) -> impl Responder {
    let user_input = form.user_input.clone();
    let progress_clone = Arc::new(progress.clone());

    // Start the main loop in a background task
    spawn(async move {
        run_main_loop(user_input, progress_clone).await;
    });

    HttpResponse::Ok().body(
        r#"
        <h1>Progress</h1>
        <pre id="progress" style="white-space: pre-wrap; word-wrap: break-word;"></pre>
        <script>
            function updateProgress() {
                fetch('/progress')
                .then(response => response.json())
                .then(data => {
                    document.getElementById('progress').innerHTML = data.output;
                    if (data.completed) {
                        // Redirect to the main page after a short delay
                        setTimeout(() => window.location.href = '/', 3000);
                    } else {
                        setTimeout(updateProgress, 2000);
                    }
                });
            }
            updateProgress();
        </script>
        "#,
    )
}

async fn run_main_loop(user_input: String, progress: Arc<web::Data<Mutex<Progress>>>) {
    let ollama = Ollama::default();

    // Use a fixed directory name for the Flask application
    let dir_name = "flask_app";
    let app_dir = Path::new(dir_name);

    // Create the directory if it doesn't exist
    if !app_dir.exists() {
        if let Err(e) = fs::create_dir(app_dir) {
            let mut progress_guard = progress.lock().unwrap();
            progress_guard.output += &format!("Error creating directory: {}\n", e);
            return;
        }
    }

    // Initial update
    {
        let mut progress_guard = progress.lock().unwrap();
        progress_guard.status = "running".to_string();
        progress_guard.iteration = 0;
        progress_guard.output = format!("Using application directory: {}\n", dir_name);
        progress_guard.completed = false;
    }

    let mut history = json!({
        "iterations": [],
        "app_directory": dir_name
    });

    // Define the structure of the Flask application
    let components = vec![
        ("app.py", "Create or update the main Flask application file (app.py) with necessary imports and app initialization."),
        ("config.py", "Create or update the configuration file (config.py) with any necessary settings."),
        ("models.py", "Create or update the models file (models.py) with any database models the application might need."),
        ("routes.py", "Create or update the routes file (routes.py) with all the necessary route handlers."),
        ("forms.py", "Create or update the forms file (forms.py) with any form classes the application might use."),
        ("templates/index.html", "Create or update the HTML template for the main page."),
        ("templates/layout.html", "Create or update the base layout HTML template."),
        ("static/style.css", "Create or update the CSS file for styling the application."),
        ("requirements.txt", "Create or update the requirements.txt file listing all necessary Python packages.")
    ];

    // Initial prompt to set the context for the LLM
    let initial_prompt = format!(
        "You are a Python Flask expert. Your task is to help build or update a multi-file Flask v3 web application based on the following request: '{}'. 
        You will be asked to generate or modify Python code for different components of the application. 
        Provide only the code or content, without any explanations or Markdown formatting. Each response should be a complete, valid file for the specified component.
        If the file already exists, incorporate the new requirements while preserving existing functionality.",
        user_input
    );

    for (i, (file_name, component_prompt)) in components.iter().enumerate() {
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

        // Update progress
        {
            let mut progress_guard = progress.lock().unwrap();
            progress_guard.iteration = i as u32 + 1;
            progress_guard.output += &format!("\nGenerating or updating {}...\n", file_name);
        }

        // LLM interaction
        let prompt = format!("{}\n\n{}", initial_prompt, component_prompt);
        let request = GenerationRequest::new(MODEL_NAME.to_string(), prompt);

        match ollama.generate(request).await {
            Ok(response) => {
                let llm_output = clean_llm_output(&response.response);

                // Create the full path for the file
                let file_path = app_dir.join(file_name);

                // Ensure parent directory exists (for templates and static files)
                if let Some(parent) = file_path.parent() {
                    if let Err(e) = fs::create_dir_all(parent) {
                        let mut progress_guard = progress.lock().unwrap();
                        progress_guard.output +=
                            &format!("Error creating directory {}: {}\n", parent.display(), e);
                        continue;
                    }
                }

                // Create or update the file with cleaned LLM output
                let file_result = match fs::write(&file_path, &llm_output) {
                    Ok(_) => format!("Created/Updated file: {}", file_path.display()),
                    Err(e) => format!(
                        "Error creating/updating file {}: {}",
                        file_path.display(),
                        e
                    ),
                };

                // Update progress with LLM output and file operation result
                {
                    let mut progress_guard = progress.lock().unwrap();
                    progress_guard.output +=
                        &format!("LLM Output for {}:\n{}\n", file_name, llm_output);
                    progress_guard.output += &format!("File operation: {}\n", file_result);
                }

                // Update history
                history["iterations"].as_array_mut().unwrap().push(json!({
                    "step": i + 1,
                    "file_name": file_name,
                    "file_operation": file_result,
                    "file_content": llm_output
                }));
            }
            Err(e) => {
                let error_msg = format!("Error in LLM interaction: {}", e);
                let mut progress_guard = progress.lock().unwrap();
                progress_guard.output += &format!("{}\n", error_msg);
            }
        }
    }

    // Log history to file
    let history_file_path = app_dir.join("generation_history.json");
    if let Err(e) = fs::write(
        &history_file_path,
        serde_json::to_string_pretty(&history).unwrap(),
    ) {
        let mut progress_guard = progress.lock().unwrap();
        progress_guard.output += &format!("Error writing history file: {}\n", e);
    }

    // Final update
    {
        let mut progress_guard = progress.lock().unwrap();
        progress_guard.status = "completed".to_string();
        progress_guard.completed = true;
        progress_guard.output += &format!(
            "\nFlask application generation/update completed! Files are in the '{}' directory.\n",
            dir_name
        );
        progress_guard.output += "Redirecting to main page in 3 seconds...";
    }
}

fn clean_llm_output(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let mut cleaned_lines = Vec::new();
    let mut in_code_block = false;

    for line in lines {
        if line.trim().starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }
        if !in_code_block || !line.trim().is_empty() {
            cleaned_lines.push(line);
        }
    }

    cleaned_lines.join("\n")
}


