use std::{
    ops::{Deref, DerefMut},
    sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard},
};

use tracing::*;

#[derive(Debug, Clone)]
pub struct DebugMetaData {
    pub timestamp: String,
    pub acquired_by_thread_id: std::thread::ThreadId,
    pub acquired_by_thread_name: String,
    pub acquired_at: String,
    pub released: Arc<RwLock<bool>>,
}

impl Default for DebugMetaData {
    fn default() -> Self {
        let current_thread = std::thread::current();

        Self {
            timestamp: chrono::Utc::now().timestamp().to_string(),
            acquired_by_thread_id: current_thread.id(),
            acquired_by_thread_name: current_thread.name().unwrap_or("unnamed").into(),
            acquired_at: caller_function_info(3),
            released: Arc::new(RwLock::new(false)),
        }
    }
}

pub struct DebugMutexGuard<'a, T> {
    inner: MutexGuard<'a, T>,
    pub released: Arc<RwLock<bool>>,
    pub creator: Arc<String>,
}

impl<'a, T> std::fmt::Debug for DebugMutexGuard<'a, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DebugMutexGuard")
            .field("released", &self.released)
            .field("creator", &self.creator)
            .finish()
    }
}

impl<'a, T> DebugMutexGuard<'a, T> {
    #[instrument(level = "debug", skip(inner, released))]
    pub fn new(
        inner: MutexGuard<'a, T>,
        released: Arc<RwLock<bool>>,
        creator: Arc<String>,
    ) -> Self {
        debug!("Guard acquired");
        Self {
            inner,
            released,
            creator,
        }
    }
}

impl<'a, T> Drop for DebugMutexGuard<'a, T> {
    #[instrument(level = "debug", skip(self))]
    fn drop(&mut self) {
        *self.released.write().unwrap() = true;
        debug!("Guard released");
    }
}

impl<'a, T> Deref for DebugMutexGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<'a, T> DerefMut for DebugMutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

#[derive(Default)]
pub struct DebugMutex<T> {
    _inner: Mutex<T>,
    pub metadata: RwLock<Vec<DebugMetaData>>,
    pub creator: Arc<String>,
}

impl<T> std::fmt::Debug for DebugMutex<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DebugMutex")
            .field("metadata", &self.metadata)
            .field("creator", &self.creator)
            .finish()
    }
}

impl<T> DebugMutex<T> {
    #[instrument(level = "debug", skip(inner))]
    pub fn new(inner: T) -> DebugMutex<T> {
        Self {
            _inner: Mutex::new(inner),
            metadata: RwLock::new(vec![]),
            creator: Arc::new(caller_function_info(2)),
        }
    }

    #[instrument(level = "debug", skip(self))]
    pub fn lock(&self) -> Result<DebugMutexGuard<'_, T>, PoisonError<MutexGuard<'_, T>>> {
        debug!("Requesting Lock...");

        {
            match self._inner.try_lock() {
                Ok(guard) => {
                    let metadata = DebugMetaData::default();
                    let released = metadata.released.clone();
                    self.metadata.write().unwrap().push(metadata);

                    return Ok(DebugMutexGuard::new(guard, released, self.creator.clone()));
                }
                Err(error) => warn!(
                    "Mutex is in use. This thread will be waiting: {:?}\n{:#?}",
                    error, self
                ),
            }
        }

        let guard = self._inner.lock()?;

        let metadata = DebugMetaData::default();
        let released = metadata.released.clone();
        self.metadata.write().unwrap().push(metadata);

        Ok(DebugMutexGuard::new(guard, released, self.creator.clone()))
    }
}

impl<T> Drop for DebugMutex<T> {
    #[instrument(level = "debug", skip(self))]
    fn drop(&mut self) {
        debug!("Mutex dropped");
    }
}

pub struct DebugRwLockReadGuard<'a, T> {
    inner: RwLockReadGuard<'a, T>,
    released: Arc<RwLock<bool>>,
    creator: Arc<String>,
}

impl<'a, T> std::fmt::Debug for DebugRwLockReadGuard<'a, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DebugRwLockReadGuard")
            .field("released", &self.released)
            .field("creator", &self.creator)
            .finish()
    }
}

impl<'a, T> DebugRwLockReadGuard<'a, T> {
    #[instrument(level = "debug", skip(inner, released))]
    pub fn new(
        inner: RwLockReadGuard<'a, T>,
        released: Arc<RwLock<bool>>,
        creator: Arc<String>,
    ) -> Self {
        debug!("Read Guard acquired");
        Self {
            inner,
            released,
            creator,
        }
    }
}

impl<'a, T> Drop for DebugRwLockReadGuard<'a, T> {
    #[instrument(level = "debug", skip(self))]
    fn drop(&mut self) {
        *self.released.write().unwrap() = true;
        debug!("Read Guard released");
    }
}

impl<'a, T> Deref for DebugRwLockReadGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

pub struct DebugRwLockWriteGuard<'a, T> {
    inner: RwLockWriteGuard<'a, T>,
    released: Arc<RwLock<bool>>,
    creator: Arc<String>,
}

impl<'a, T> std::fmt::Debug for DebugRwLockWriteGuard<'a, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DebugRwLockWriteGuard")
            .field("released", &self.released)
            .field("creator", &self.creator)
            .finish()
    }
}

impl<'a, T> DebugRwLockWriteGuard<'a, T> {
    #[instrument(level = "debug", skip(inner, released))]
    pub fn new(
        inner: RwLockWriteGuard<'a, T>,
        released: Arc<RwLock<bool>>,
        creator: Arc<String>,
    ) -> Self {
        debug!("Write Guard acquired");
        Self {
            inner,
            released,
            creator,
        }
    }
}

impl<'a, T> Drop for DebugRwLockWriteGuard<'a, T> {
    #[instrument(level = "debug", skip(self))]
    fn drop(&mut self) {
        *self.released.write().unwrap() = true;
        debug!("Write Guard released");
    }
}

impl<'a, T> Deref for DebugRwLockWriteGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<'a, T> DerefMut for DebugRwLockWriteGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

#[derive(Default)]

pub struct DebugRwLock<T> {
    _inner: RwLock<T>,
    pub read_metadata: RwLock<Vec<DebugMetaData>>,
    pub write_metadata: RwLock<Vec<DebugMetaData>>,
    pub creator: Arc<String>,
}

impl<T> std::fmt::Debug for DebugRwLock<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DebugRwLock")
            .field("read_metadata", &self.read_metadata)
            .field("write_metadata", &self.write_metadata)
            .field("creator", &self.creator)
            .finish()
    }
}

impl<T> DebugRwLock<T> {
    #[instrument(level = "debug", skip(inner))]
    pub fn new(inner: T) -> DebugRwLock<T> {
        Self {
            _inner: RwLock::new(inner),
            read_metadata: RwLock::new(vec![]),
            write_metadata: RwLock::new(vec![]),
            creator: Arc::new(caller_function_info(2)),
        }
    }

    #[instrument(level = "debug", skip(self))]
    pub fn read(&self) -> Result<DebugRwLockReadGuard<'_, T>, PoisonError<RwLockReadGuard<'_, T>>> {
        debug!("Read Requesting Lock...");

        {
            match self._inner.try_read() {
                Ok(guard) => {
                    let metadata = DebugMetaData::default();
                    let released = metadata.released.clone();
                    let mut metadatas = self.read_metadata.write().unwrap();

                    *metadatas = metadatas
                        .iter()
                        .filter(|&metadata| !*metadata.released.read().unwrap())
                        .cloned()
                        .collect();
                    metadatas.push(metadata);

                    return Ok(DebugRwLockReadGuard::new(
                        guard,
                        released,
                        self.creator.clone(),
                    ));
                }
                Err(error) => {
                    debug!(
                        "RwLock is in use. This thread will be waiting: {:?}\n{:#?}",
                        error, self
                    )
                }
            }
        }

        let guard = self._inner.read()?;

        let metadata = DebugMetaData::default();
        let released = metadata.released.clone();
        self.read_metadata.write().unwrap().push(metadata);

        Ok(DebugRwLockReadGuard::new(
            guard,
            released,
            self.creator.clone(),
        ))
    }

    #[instrument(level = "debug", skip(self))]
    pub fn write(
        &self,
    ) -> Result<DebugRwLockWriteGuard<'_, T>, PoisonError<RwLockWriteGuard<'_, T>>> {
        debug!("Write Requesting Lock...");

        {
            match self._inner.try_write() {
                Ok(guard) => {
                    let metadata = DebugMetaData::default();
                    let released = metadata.released.clone();
                    self.write_metadata.write().unwrap().push(metadata);

                    return Ok(DebugRwLockWriteGuard::new(
                        guard,
                        released,
                        self.creator.clone(),
                    ));
                }
                Err(error) => {
                    debug!(
                        "RwLock is in use. This thread will be waiting: {:?}\n{:#?}",
                        error, self
                    )
                }
            }
        }

        let guard = self._inner.write()?;

        let metadata = DebugMetaData::default();
        let released = metadata.released.clone();
        self.write_metadata.write().unwrap().push(metadata);

        Ok(DebugRwLockWriteGuard::new(
            guard,
            released,
            self.creator.clone(),
        ))
    }
}

impl<T> Drop for DebugRwLock<T> {
    #[instrument(level = "debug", skip(self))]
    fn drop(&mut self) {
        debug!("RwLock dropped");
    }
}

#[derive(Debug, Default)]
pub struct DebugArc<T>(Arc<T>);

impl<T> DebugArc<T> {
    #[instrument(level = "debug", skip(inner))]
    pub fn new(inner: T) -> DebugArc<T> {
        DebugArc(Arc::new(inner))
    }
}

impl<T> Clone for DebugArc<T> {
    fn clone(&self) -> Self {
        debug!(
            "Arc Cloned. Refs: {}, WeakRefs: {}",
            Arc::strong_count(&self.0),
            Arc::weak_count(&self.0)
        );
        Self(self.0.clone())
    }
}

impl<T> Drop for DebugArc<T> {
    #[instrument(level = "debug", skip(self))]
    fn drop(&mut self) {
        debug!(
            "Arc Dropped. Refs: {}, WeakRefs: {}",
            Arc::strong_count(&self.0),
            Arc::weak_count(&self.0)
        );
    }
}

impl<T> Deref for DebugArc<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

fn caller_function_info(frames_ago: usize) -> String {
    backtrace::Backtrace::new()
        .frames()
        .get(frames_ago)
        .and_then(|frame| {
            frame.symbols().get(0).map(|symbol| {
                format!(
                    "{}:{}:{} [{}]",
                    symbol
                        .filename()
                        .map(|v| v.to_string_lossy().to_string())
                        .unwrap_or("?".into()),
                    symbol.lineno().map(|v| v.to_string()).unwrap_or("?".into()),
                    symbol.colno().map(|v| v.to_string()).unwrap_or("?".into()),
                    symbol.name().map(|v| v.to_string()).unwrap_or("?".into()),
                )
            })
        })
        .unwrap_or("unknown".to_string())
}
