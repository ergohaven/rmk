use embassy_time::{Duration, with_timeout};
use embedded_storage_async::nor_flash::{ErrorType, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash};

/// How long one write, or erasing one page, may take. With BLE on nRF52 the SoftDevice
/// Controller runs each in a radio timeslot: an erase takes about 85 ms per page, a
/// write well under one. A timeslot that is never granted waits out MPSL's 256 s
/// request timeout, and the storage task, with everyone waiting on its queue, with it.
pub(crate) const FLASH_OP_TIMEOUT_PER_PAGE: Duration = Duration::from_secs(2);

/// Fails an erase or write that overruns [`FLASH_OP_TIMEOUT_PER_PAGE`] per page. The
/// map handles that like any flash error: the item is not stored, and the next call
/// rebuilds the cache the interrupted one left dirty.
pub(crate) struct TimeoutFlash<F>(pub(crate) F);

#[derive(Debug)]
pub(crate) enum TimeoutFlashError<E> {
    Flash(E),
    Timeout,
}

impl<E: NorFlashError> NorFlashError for TimeoutFlashError<E> {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            Self::Flash(e) => e.kind(),
            Self::Timeout => NorFlashErrorKind::Other,
        }
    }
}

impl<F: ErrorType> ErrorType for TimeoutFlash<F> {
    type Error = TimeoutFlashError<F::Error>;
}

impl<F: ReadNorFlash> ReadNorFlash for TimeoutFlash<F> {
    const READ_SIZE: usize = F::READ_SIZE;

    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.0.read(offset, bytes).await.map_err(TimeoutFlashError::Flash)
    }

    fn capacity(&self) -> usize {
        self.0.capacity()
    }
}

impl<F: NorFlash> NorFlash for TimeoutFlash<F> {
    const WRITE_SIZE: usize = F::WRITE_SIZE;
    const ERASE_SIZE: usize = F::ERASE_SIZE;

    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        let pages = (to.saturating_sub(from) as usize).div_ceil(F::ERASE_SIZE.max(1)).max(1);
        with_timeout(FLASH_OP_TIMEOUT_PER_PAGE * pages as u32, self.0.erase(from, to))
            .await
            .map_err(|_| TimeoutFlashError::Timeout)?
            .map_err(TimeoutFlashError::Flash)
    }

    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        with_timeout(FLASH_OP_TIMEOUT_PER_PAGE, self.0.write(offset, bytes))
            .await
            .map_err(|_| TimeoutFlashError::Timeout)?
            .map_err(TimeoutFlashError::Flash)
    }
}

#[cfg(test)]
mod tests {
    use embassy_time::Instant;

    use super::*;
    use crate::test_support::test_block_on as block_on;

    /// A flash whose erases and writes never finish, like a timeslot that is never granted.
    struct StalledFlash;

    impl ErrorType for StalledFlash {
        type Error = NorFlashErrorKind;
    }

    impl ReadNorFlash for StalledFlash {
        const READ_SIZE: usize = 1;

        async fn read(&mut self, _offset: u32, _bytes: &mut [u8]) -> Result<(), Self::Error> {
            Ok(())
        }

        fn capacity(&self) -> usize {
            8_192
        }
    }

    impl NorFlash for StalledFlash {
        const WRITE_SIZE: usize = 4;
        const ERASE_SIZE: usize = 4_096;

        async fn erase(&mut self, _from: u32, _to: u32) -> Result<(), Self::Error> {
            core::future::pending().await
        }

        async fn write(&mut self, _offset: u32, _bytes: &[u8]) -> Result<(), Self::Error> {
            core::future::pending().await
        }
    }

    #[test]
    fn stalled_operations_fail_after_their_page_budget() {
        block_on(async {
            let mut flash = TimeoutFlash(StalledFlash);

            let start = Instant::now();
            assert!(matches!(flash.write(0, &[0; 4]).await, Err(TimeoutFlashError::Timeout)));
            assert!(start.elapsed() >= FLASH_OP_TIMEOUT_PER_PAGE);

            // A two-page erase gets two pages' worth.
            let start = Instant::now();
            assert!(matches!(flash.erase(0, 8_192).await, Err(TimeoutFlashError::Timeout)));
            assert!(start.elapsed() >= FLASH_OP_TIMEOUT_PER_PAGE * 2);
        });
    }
}
