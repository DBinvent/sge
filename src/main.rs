#[macro_use]
extern crate slog;

use rdbm::dbm_yml::history_table_name;


use slog::{Drain, Logger};
use sloggers::terminal::{Destination, TerminalLoggerBuilder};
use sloggers::types::Severity;
use sloggers::Build;

#[inline]
pub fn logger() -> Logger {
	logger2(Severity::Info)
}

#[inline]
pub fn logger2(level: Severity) -> Logger {
	let mut builder = TerminalLoggerBuilder::new();
	builder.channel_size(1024);
	builder.destination(Destination::Stdout);
	builder.level(level);
	builder.build().unwrap()
}

fn main() {
	info!(logger(), "hi: {}", history_table_name());
}
