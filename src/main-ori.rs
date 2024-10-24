use actix_files as afs;
use actix_web::{web, App, HttpResponse, HttpServer, Responder};
use ollama_rs::{generation::completion::request::GenerationRequest, Ollama};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::runtime::Runtime;

use std::{
    fs::{self, File},
    io::Write,
    path::Path,
    sync::Mutex,
};
use tokio::time::sleep;

const MODEL_NAME: &str = "llama3.2";
const LOG_FILE: &str = "actix_app_builder_log.json";

#[derive(Clone, Serialize, Deserialize)]
struct Progress {
    status: String,
    iteration: u32,
    max_iteration: u32,
    output: String,
    completed: bool,
}

fn init_progress() -> Progress {
    Progress {
        status: "idle".to_string(),
        iteration: 0,
        max_iteration: 50,
        output: String::new(),
        completed: false,
    }
}

fn create_directory(path: &str) -> String {
    match fs::create_dir_all(path) {
        Ok(_) => format!("Created directory: {}", path),
        Err(e) => format!("Error creating directory {}: {}", path, e),
    }
}

fn create_or_update_file(path: &str, content: &str) -> String {
    match fs::write(path, content) {
        Ok(_) => format!("Created/Updated file: {}", path),
        Err(e) => format!("Error creating/updating file {}: {}", path, e),
    }
}

fn fetch_code(file_path: &str) -> String {
    match fs::read_to_string(file_path) {
        Ok(code) => code,
        Err(e) => format!("Error fetching code from {}: {}", file_path, e),
    }
}

fn log_to_file(history: &serde_json::Value) {
    if let Ok(mut file) = File::create(LOG_FILE) {
        if let Err(e) = file.write(serde_json::to_string_pretty(history).unwrap().as_bytes()) {
            eprintln!("Error writing to log file: {}", e);
        }
    }
}

async fn home(progress: web::Data<Mutex<Progress>>) -> impl Responder {
    let index_path = Path::new("templates/index.html");

    // lock progress current state
    let progress_data = progress.lock().unwrap();

    let content = if index_path.exists() {
        match fs::read_to_string(index_path) {
            Ok(contents) => contents,
            Err(_) => return HttpResponse::InternalServerError().body("Error reading index.html"),
        }
    } else {
        let status_html = if progress_data.status == "running" {
            format!(
                r#"
                <h2>Current Progress</h2>
                <p>Status: {}</p>
                <p>Iteration: {}{}</p>
                <pre>{}</pre>
                "#,
                progress_data.status,
                progress_data.iteration,
                progress_data.max_iteration,
                progress_data.output,
            )
        } else {
            String::new()
        };

        format!(
            r#"
            <h1>Flask App Builder</h1>
            {}
            <form method="post">
                <label for="user_input">Describe the Flask app you want to create:</label><br>
                <textarea id="user_input" name="user_input"></textarea><br><br>
                <input type="submit" value="Submit">
            </form>
            "#,
            status_html
        )
    };

    HttpResponse::Ok().content_type("text/html").body(content)
}

async fn get_progress(progress: web::Data<Mutex<Progress>>) -> impl Responder {
    let progress = progress.lock().unwrap();
    web::Json(progress.clone())
}

#[derive(Deserialize)]
struct UserInput {
    user_input: String,
}

async fn handle_post(
    form: web::Form<UserInput>,
    progress: web::Data<Mutex<Progress>>,
) -> impl Responder {
    let user_input = form.user_input.clone();

    // create a tokio runtime
    let rt = Runtime::new().unwrap();

    // start the main loop in a separate thread
    let progress_clone = progress.clone();
    std::thread::spawn(move || {
        rt.block_on(run_main_loop(user_input, progress_clone));
    });

    HttpResponse::Ok().body(
        r#"
        <h1>Progress</h1>
        <pre id="progress"></pre>
        <script>
            setInterval(function() {
                fetch('/progress')
                .then(response => response.json())
                .then(data => {
                    document.getElementById('progress').innerHTML = data.output;
                    if (data.completed) {
                        document.getElementById('refresh-btn').style.display = 'block';
                    }
                });
            }, 2000);
        </script>
        <button id="refresh-btn" style="display:none;" onclick="location.reload();">Refresh Page</button>
        "#
    )
}

async fn run_main_loop(user_input: String, progress: web::Data<Mutex<Progress>>) {
    let ollama = Ollama::default();

    // init update
    {
        let mut progress_guard = progress.lock().unwrap();
        progress_guard.status = "running".to_string();
        progress_guard.iteration = 0;
        progress_guard.output = String::new();
        progress_guard.completed = false;
    }

    let mut history = json!({
        "iterations": []
    });

    // defined  the structure of the Flask application
    let components = vec![
        ("app.py", "Create the main Flask application file (app.py) with necessary imports and app initialization."),
        ("config.py", "Create a configuration file (config.py) with any necessary settings."),
        ("models.py", "Create a models file (models.py) with any database models the application might need."),
        ("routes.py", "Create a routes file (routes.py) with all the necessary route handlers."),
        ("forms.py", "Create a forms file (forms.py) with any form classes the application might use."),
        ("templates/index.html", "Create an HTML template for the main page."),
        ("templates/layout.html", "Create a base layout HTML template."),
        ("static/style.css", "Create a CSS file for styling the application."),
        ("requirements.txt", "Create a requirements.txt file listing all necessary Python packages."),
    ];

    let initial_prompt = format!(
        "You are an expert Flask developer. Your task is to help build a multi-file Flask web application based on the following request: '{}'.
        You will be asked to generate Python code for different components of the application.
        Provide only the code or content, without any explanations. Each response should be a complete, valid file for the specified component.",
        user_input
    );

    for (i, (file_name, component_prompt)) in components.iter().enumerate() {
        sleep(tokio::time::Duration::from_secs(1)).await;

        // update progress
        {
            let mut progress_guard = progress.lock().unwrap();
            progress_guard.iteration = i as u32 + 1;
            progress_guard.output += &format!("Generating {}...\n", file_name);
        }

        // LLM interaction
        let prompt = format!("{}\n\n{}", initial_prompt, component_prompt);
        let request = GenerationRequest::new(MODEL_NAME.to_string(), prompt);

        match ollama.generate(request).await {
            Ok(response) => {
                let llm_output = response.response;

                let file_result = create_or_update_file(file_name, &llm_output);
                let fetched_content = fetch_code(file_name);

                {
                    let mut progress_guard = progress.lock().unwrap();
                    progress_guard.output += &format!("File operation: {}\n", file_result);
                    progress_guard.output += &format!(
                        "
                        Generated content for {}:\n{}\n
                    ",
                        file_name, fetched_content
                    );
                }

                // update history
                history["iterations"].as_array_mut().unwrap().push(json!({
                "step": i + 1,
                "file_name": file_name,
                "file_operation": file_result,
                "file_content": fetched_content
                }));
            }
            Err(e) => {
                let error_msg = format!("Error in LLM iteraction: {}", e);
                let mut progress_guard = progress.lock().unwrap();
                progress_guard.output += &format!("{}\n", error_msg);
            }
        }
    }

    log_to_file(&history);

    // final update
    {
        let mut progress_guard = progress.lock().unwrap();
        progress_guard.status = "completed".to_string();
        progress_guard.completed = true;
        progress_guard.output += "Flask application generation completed!";
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    create_directory("templates");
    create_directory("static");
    create_directory("routes");

    let progress = web::Data::new(Mutex::new(init_progress()));

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
