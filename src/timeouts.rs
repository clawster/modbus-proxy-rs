//! Timeouts guarding against stalled peers. Tests use short values.

use std::time::Duration;

/// Max time to connect to, write to or read from the modbus device. An
/// unresponsive device must not wedge the device task (and with it every
/// client task and socket).
#[cfg(not(test))]
pub(crate) const DEVICE_IO_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(test)]
pub(crate) const DEVICE_IO_TIMEOUT: Duration = Duration::from_millis(200);

/// Max time a client request may wait to be queued and answered.
#[cfg(not(test))]
pub(crate) const CLIENT_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
#[cfg(test)]
pub(crate) const CLIENT_REQUEST_TIMEOUT: Duration = Duration::from_millis(300);

/// Pause after a failed accept (e.g. out of file descriptors) before retrying.
#[cfg(not(test))]
pub(crate) const ACCEPT_RETRY_DELAY: Duration = Duration::from_millis(100);
#[cfg(test)]
pub(crate) const ACCEPT_RETRY_DELAY: Duration = Duration::from_millis(10);
