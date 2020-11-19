extern crate pgcat;

mod init;

#[macro_use]
extern crate slog;

use crate::init::logger;

#[test]
fn integration_test_1() {
    let log = logger();

    info!(log, "ok");
}
