use bicho::Client;

#[tauri::command]
async fn parse_settings(
    input: String,
    tracker: tauri::State<'_, Client>,
) -> Result<serde_json::Value, String> {
    if input.len() > 64 * 1024 {
        return Err("Settings exceed the size limit".into());
    }
    match serde_json::from_str(&input) {
        Ok(settings) => Ok(settings),
        Err(error) => {
            if let Err(delivery) = tracker.capture_error(&error).await {
                eprintln!("Bicho delivery failed: {delivery}");
            }
            Err("Could not parse settings".into())
        }
    }
}

pub fn configure<R: tauri::Runtime>(
    builder: tauri::Builder<R>,
    tracker: Client,
) -> tauri::Builder<R> {
    builder
        .manage(tracker)
        .invoke_handler(tauri::generate_handler![parse_settings])
}
