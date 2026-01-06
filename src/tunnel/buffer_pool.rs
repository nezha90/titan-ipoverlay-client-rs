use once_cell::sync::Lazy;
use std::sync::Mutex;

/// Buffer size: 32KB - optimized for most network scenarios
const BUFFER_SIZE: usize = 32 * 1024;

/// Maximum number of buffers in the pool
const MAX_POOL_SIZE: usize = 128;

/// Global buffer pool using sync::Pool pattern
static BUFFER_POOL: Lazy<Mutex<Vec<Vec<u8>>>> = Lazy::new(|| Mutex::new(Vec::new()));

/// A RAII guard that returns the buffer to the pool when dropped
pub struct PooledBuffer {
    buffer: Option<Vec<u8>>,
}

impl PooledBuffer {
    /// Get a mutable reference to the underlying buffer
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        self.buffer.as_mut().unwrap().as_mut_slice()
    }

    /// Get the buffer length
    pub fn len(&self) -> usize {
        self.buffer.as_ref().unwrap().len()
    }

    /// Check if buffer is empty
    pub fn is_empty(&self) -> bool {
        self.buffer.as_ref().unwrap().is_empty()
    }

    /// Get a slice of the buffer up to n bytes
    pub fn slice(&self, n: usize) -> &[u8] {
        &self.buffer.as_ref().unwrap()[..n]
    }
}

impl Drop for PooledBuffer {
    fn drop(&mut self) {
        if let Some(mut buf) = self.buffer.take() {
            // Reset buffer for reuse
            buf.clear();
            buf.resize(BUFFER_SIZE, 0);

            // Return to pool if not full
            let mut pool = BUFFER_POOL.lock().unwrap();
            if pool.len() < MAX_POOL_SIZE {
                pool.push(buf);
            }
            // Otherwise, let it drop and be deallocated
        }
    }
}

/// Acquire a buffer from the pool
/// 
/// This function will reuse an existing buffer from the pool if available,
/// or allocate a new one if the pool is empty.
/// 
/// The buffer is automatically returned to the pool when the PooledBuffer is dropped.
pub fn acquire() -> PooledBuffer {
    let mut pool = BUFFER_POOL.lock().unwrap();
    
    let buffer = if let Some(buf) = pool.pop() {
        // Reuse existing buffer
        buf
    } else {
        // Allocate new buffer
        vec![0u8; BUFFER_SIZE]
    };

    PooledBuffer {
        buffer: Some(buffer),
    }
}

/// Get current pool statistics (for monitoring/debugging)
pub fn pool_stats() -> (usize, usize) {
    let pool = BUFFER_POOL.lock().unwrap();
    (pool.len(), MAX_POOL_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buffer_pool_acquire_and_return() {
        let buf1 = acquire();
        assert_eq!(buf1.len(), BUFFER_SIZE);
        drop(buf1);

        let (available, _) = pool_stats();
        assert_eq!(available, 1);
    }

    #[test]
    fn test_buffer_pool_reuse() {
        // Acquire and drop multiple buffers
        for _ in 0..10 {
            let _buf = acquire();
        }

        let (available, max) = pool_stats();
        assert!(available <= max);
        assert!(available > 0);
    }

    #[test]
    fn test_buffer_slice() {
        let mut buf = acquire();
        let slice = buf.as_mut_slice();
        slice[0] = 42;
        slice[1] = 43;

        assert_eq!(buf.slice(2), &[42, 43]);
    }
}
