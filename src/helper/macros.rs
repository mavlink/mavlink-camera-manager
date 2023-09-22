#[macro_export]
macro_rules! force_lock {
    ($mutex:expr) => {{
        let mut tries: u64 = 0;
        let start = std::time::Instant::now();

        loop {
            let secs_blocked = start.elapsed().as_secs();

            match $mutex.try_lock() {
                Ok(guard) => {
                    if tries != 0 {
                        debug!(tries, secs_blocked, "guard acquired");
                    }

                    break guard;
                }
                Err(std::sync::TryLockError::Poisoned(error)) => {
                    error!(
                        tries,
                        secs_blocked, "Trying to recover from a thread lock: {error:#?}"
                    );

                    break error.into_inner();
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    if start.elapsed() > std::time::Duration::from_secs(10) {
                        debug!(tries, secs_blocked, "guard blocked");
                    }

                    std::thread::sleep(std::time::Duration::from_millis(1));
                    tries += 1;
                }
            }
        }
    }};
}

#[macro_export]
macro_rules! force_read {
    ($rwlock:expr) => {{
        let mut tries: u64 = 0;
        let start = std::time::Instant::now();

        loop {
            let secs_blocked = start.elapsed().as_secs();

            match $rwlock.try_read() {
                Ok(guard) => {
                    if tries != 0 {
                        debug!(tries, secs_blocked, "guard acquired");
                    }

                    break guard;
                }
                Err(std::sync::TryLockError::Poisoned(error)) => {
                    error!(
                        tries,
                        secs_blocked, "Trying to recover from a thread lock: {error:#?}"
                    );

                    break error.into_inner();
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    if start.elapsed() > std::time::Duration::from_secs(10) {
                        debug!(tries, secs_blocked, "guard blocked");
                    }

                    std::thread::sleep(std::time::Duration::from_millis(1));
                    tries += 1;
                }
            }
        }
    }};
}

#[macro_export]
macro_rules! force_write {
    ($rwlock:expr) => {{
        let mut tries: u64 = 0;
        let start = std::time::Instant::now();
        let mut current = std::time::Instant::now();

        loop {
            let secs_blocked = start.elapsed().as_secs();

            match $rwlock.try_write() {
                Ok(guard) => {
                    if tries != 0 {
                        debug!(tries, secs_blocked, "guard acquired");
                    }

                    break guard;
                }
                Err(std::sync::TryLockError::Poisoned(error)) => {
                    error!(
                        tries,
                        secs_blocked, "Trying to recover from a thread lock: {error:#?}"
                    );

                    break error.into_inner();
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    if current.elapsed() > std::time::Duration::from_secs(10) {
                        current = std::time::Instant::now();
                        debug!(tries, secs_blocked, "guard blocked");
                    }

                    std::thread::sleep(std::time::Duration::from_millis(1));
                    tries += 1;
                }
            }
        }
    }};
}

#[macro_export]
macro_rules! force_lock_async {
    ($mutex:expr) => {{
        let mut tries: u64 = 0;
        let start = std::time::Instant::now();

        loop {
            let secs_blocked = start.elapsed().as_secs();

            match $mutex.try_lock() {
                Ok(guard) => {
                    if tries != 0 {
                        debug!(tries, secs_blocked, "guard acquired");
                    }

                    break guard;
                }
                Err(std::sync::TryLockError::Poisoned(error)) => {
                    error!(
                        tries,
                        secs_blocked, "Trying to recover from a thread lock: {error:#?}"
                    );

                    break error.into_inner();
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    if start.elapsed() > std::time::Duration::from_secs(10) {
                        debug!(tries, secs_blocked, "guard blocked");
                    }

                    tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
                    tries += 1;
                }
            }
        }
    }};
}

#[macro_export]
macro_rules! force_read_async {
    ($rwlock:expr) => {{
        let mut tries: u64 = 0;
        let start = std::time::Instant::now();
        let mut current = std::time::Instant::now();

        loop {
            let secs_blocked = start.elapsed().as_secs();

            match $rwlock.try_read() {
                Ok(guard) => {
                    if tries != 0 {
                        debug!(tries, secs_blocked, "guard acquired");
                    }

                    break guard;
                }
                Err(std::sync::TryLockError::Poisoned(error)) => {
                    error!(
                        tries,
                        secs_blocked, "Trying to recover from a thread lock: {error:#?}"
                    );

                    break error.into_inner();
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    if current.elapsed() > std::time::Duration::from_secs(10) {
                        current = std::time::Instant::now();
                        debug!(tries, secs_blocked, "guard blocked");
                    }

                    tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
                    tries += 1;
                }
            }
        }
    }};
}

#[macro_export]
macro_rules! force_write_async {
    ($rwlock:expr) => {{
        let mut tries: u64 = 0;
        let start = std::time::Instant::now();
        let mut current = std::time::Instant::now();

        loop {
            let secs_blocked = start.elapsed().as_secs();

            match $rwlock.try_write() {
                Ok(guard) => {
                    if tries != 0 {
                        debug!(tries, secs_blocked, "guard acquired");
                    }

                    break guard;
                }
                Err(std::sync::TryLockError::Poisoned(error)) => {
                    error!(
                        tries,
                        secs_blocked, "Trying to recover from a thread lock: {error:#?}"
                    );

                    break error.into_inner();
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    if current.elapsed() > std::time::Duration::from_secs(10) {
                        current = std::time::Instant::now();
                        debug!(tries, secs_blocked, "guard blocked");
                    }

                    tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
                    tries += 1;
                }
            }
        }
    }};
}
