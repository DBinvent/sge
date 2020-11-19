extern crate pgcat;

use slog::Logger;
use postgres::error::Severity;

pub fn logger() -> Logger {
    let mut builder = TerminalLoggerBuilder::new();
    builder.level(Severity::Debug);
    builder.destination(Destination::Stderr);
    builder.build().unwrap()
}

// add common test logic here
