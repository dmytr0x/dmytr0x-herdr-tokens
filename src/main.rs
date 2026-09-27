use clap::Parser;
#[tokio::main]
async fn main() -> std::process::ExitCode {
    match herdr_tokens::execute(herdr_tokens::Cli::parse()).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(category = %e, "command failed");
            eprintln!("{e}");
            std::process::ExitCode::from(if e.downcast_ref::<herdr_tokens::Invalid>().is_some() {
                2
            } else {
                1
            })
        }
    }
}
