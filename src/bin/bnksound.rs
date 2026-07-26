use std::process::ExitCode;

/// The counting allocator the perf gate reads, installed only when measuring.
#[cfg(feature = "perf-alloc")]
#[global_allocator]
static GLOBAL: bnksound::dev::alloc::Counting = bnksound::dev::alloc::Counting;

fn main() -> ExitCode {
    // A dev flag runs its command and stops. Without the feature there are no
    // such flags, and this is the window opener it has always been.
    #[cfg(feature = "dev")]
    {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if let Some(result) = bnksound::dev::run(&args) {
            return match result {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("bnksound: {e}");
                    ExitCode::FAILURE
                }
            };
        }
    }

    match bnksound::native::app::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("bnksound: {e}");
            ExitCode::FAILURE
        }
    }
}
