use session_lib::Session;

#[tokio::main]
async fn main() {
    let s = Session::open(1);
    println!("[tokio-app] using session {}", s.id);
    drop(s);

    // Give the detached cleanup task a moment to run before the runtime shuts
    // down. Real code should await a shutdown signal instead of sleeping.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
}
