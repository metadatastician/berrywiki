//! Thin shell over `berrywiki_cli::run`.

use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let code = match berrywiki_cli::run(&args, &mut lock) {
        Ok(code) => code,
        Err(e) => {
            // Only reached if writing to stdout itself failed.
            let _ = writeln!(std::io::stderr(), "berrywiki: I/O error: {e}");
            2
        }
    };
    let _ = lock.flush();
    std::process::exit(code);
}
