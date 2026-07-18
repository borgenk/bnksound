use std::process::ExitCode;

fn main() -> ExitCode {
    match bnksound::native::app::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("bnksound: {e}");
            ExitCode::FAILURE
        }
    }
}
