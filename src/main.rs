mod backend;
mod handlers;
mod input;
mod keyboard;
mod shell;
mod state;

fn main() {
    if let Ok(filter) = tracing_subscriber::EnvFilter::try_from_default_env() {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new("warn"))
            .init();
    }

    if let Err(err) = backend::run() {
        tracing::error!(%err, "smallstepwm stopped");
        eprintln!("smallstepwm: {err}");
        std::process::exit(1);
    }
}
