use session_lib::Session;

fn main() {
    smol::block_on(async {
        let s = Session::open(1);
        println!("[smol-app] using session {}", s.id);
        drop(s);

        // Give the detached cleanup task a moment to run before block_on
        // returns. Real code should await a shutdown signal instead of
        // sleeping.
        smol::Timer::after(std::time::Duration::from_millis(50)).await;
    });
}
