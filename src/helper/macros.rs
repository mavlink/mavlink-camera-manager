#[macro_export]
macro_rules! lock_or_return_error {
    ($mutex:expr) => {
        match $mutex.lock() {
            Ok(guard) => guard,
            Err(error) => return error!("Failed locking a Mutex. Reason: {error}"),
        }
    };
}
