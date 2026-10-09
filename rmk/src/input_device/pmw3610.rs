//! PMW3610 Low-Power Mouse Sensor Driver
//!
//! Ported from the Zephyr driver implementation:
//! https://github.com/zephyrproject-rtos/zephyr/blob/d31c6e95033fd6b3763389edba6a655245ae1328/drivers/input/input_pmw3610.c

use embassy_time::{Duration, Instant, Timer};
use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal_async::digital::Wait;
use embedded_hal_async::spi::SpiBus;

pub use crate::driver::bitbang_spi::{BitBangError, BitBangSpiBus};
use crate::input_device::pointing::{InitState, MotionData, PointingDevice, PointingDriver, PointingDriverError};

// ============================================================================
// Page 0 registers
// ============================================================================
const PMW3610_PROD_ID: u8 = 0x00;
const PMW3610_MOTION: u8 = 0x02;
const PMW3610_DELTA_XY_H: u8 = 0x05;
const PMW3610_PERFORMANCE: u8 = 0x11;
const PMW3610_BURST_READ: u8 = 0x12;
const PMW3610_RUN_DOWNSHIFT: u8 = 0x1b;
const PMW3610_REST1_RATE: u8 = 0x1c;
const PMW3610_REST1_DOWNSHIFT: u8 = 0x1d;
const PMW3610_OBSERVATION1: u8 = 0x2d;
const PMW3610_SMART_MODE: u8 = 0x32;
const PMW3610_POWER_UP_RESET: u8 = 0x3a;
const PMW3610_SHUTDOWN: u8 = 0x3b;
const PMW3610_SPI_CLK_ON_REQ: u8 = 0x41;
const PMW3610_SPI_PAGE0: u8 = 0x7f;

// ============================================================================
// Page 1 registers
// ============================================================================
const PMW3610_RES_STEP: u8 = 0x05;
const PMW3610_SPI_PAGE1: u8 = 0x7f;

// ============================================================================
// Burst register offsets
// ============================================================================
const BURST_MOTION: usize = 0;
const BURST_DELTA_X_L: usize = 1;
const BURST_DELTA_Y_L: usize = 2;
const BURST_DELTA_XY_H: usize = 3;
const BURST_SQUAL: usize = 4;
const BURST_SHUTTER_HI: usize = 5;
const BURST_SHUTTER_LO: usize = 6;

const BURST_DATA_LEN_NORMAL: usize = BURST_DELTA_XY_H + 1;
const BURST_DATA_LEN_SMART: usize = BURST_SHUTTER_LO + 1;

// ============================================================================
// Init sequence values
// ============================================================================
const OBSERVATION1_INIT_MASK: u8 = 0x0f;
const PERFORMANCE_INIT: u8 = 0x0d;
const RUN_DOWNSHIFT_INIT: u8 = 0x04;
const REST1_RATE_INIT: u8 = 0x04;
const REST1_DOWNSHIFT_INIT: u8 = 0x0f;

// ============================================================================
// Constants
// ============================================================================
const PRODUCT_ID_PMW3610: u8 = 0x3e;
const SPI_WRITE: u8 = 0x80;
const MOTION_STATUS_MOTION: u8 = 0x80;
const SPI_CLOCK_ON_REQ_ON: u8 = 0xba;
const SPI_CLOCK_ON_REQ_OFF: u8 = 0xb5;
const RES_STEP_SWAP_XY_BIT: u8 = 7;
const RES_STEP_INV_X_BIT: u8 = 6;
const RES_STEP_INV_Y_BIT: u8 = 5;
const RES_STEP_RES_MASK: u8 = 0x1f;
const PERFORMANCE_FMODE_MASK: u8 = 0x0f << 4;
const PERFORMANCE_FMODE_NORMAL: u8 = 0x00 << 4;
const PERFORMANCE_FMODE_FORCE_AWAKE: u8 = 0x0f << 4;
const POWER_UP_RESET_VAL: u8 = 0x5a;
const SHUTDOWN_ENABLE: u8 = 0xe7;
const SPI_PAGE0_1: u8 = 0xff;
const SPI_PAGE1_0: u8 = 0x00;
const SHUTTER_SMART_THRESHOLD: u16 = 45;
const SMART_MODE_ENABLE: u8 = 0x00;
const SMART_MODE_DISABLE: u8 = 0x80;

const PMW3610_DATA_SIZE_BITS: usize = 12;

// Timing constants
const RESET_DELAY_MS: u64 = 10;
const INIT_OBSERVATION_DELAY_MS: u64 = 10;
const CLOCK_ON_DELAY_US: u64 = 300;

// SPI timing constants (from PMW3610 datasheet)
/// NCS to SCLK active;
/// Delay from last NCS falling edge to 1st SCK rising edge
const T_NCS_SCLK_NS: u64 = 120;
/// SPI read address-data delay;
/// from rising SCLK for last bit of the address byte, to falling SCLK for the 1st bit of data being read.
const T_SRAD_US: u64 = 4;
/// SPI time between read and subsequent commands;
/// from rising SCLK for last bit of the 1st data byte, to falling SCLK for the 1st bit of data being read.
const T_SRX_NS: u64 = 250;
/// SPI time between write command;
/// From rising SCLK for last bit of the first data byte, to rising SCLK for last bit of the second data byte.
/// It's actually 20 us before read and 30 us before write, but we don't distinguish between write and read commands here, and use the larger of the two.
const T_SWX_US: u64 = 30;
/// SCLK to NCS inactive for SDIO write;
/// From last SCLK falling edge to NCS rising edge, for valid SDIO data transfer
const T_SCLK_NCS_W_US: u64 = 10;
/// SCLK to NCS inactive for SDIO read;
/// From last SCLK falling edge to NCS rising edge, for valid SDIO data transfer
const T_SCLK_NCS_R_NS: u64 = 120;
/// NCS inactive after motion burst;
/// Minimum NCS inactive time after motion burst before next SPI usage
const T_BEXIT_NS: u64 = 250;

// Resolution constants
const RES_STEP: u16 = 200;
const RES_MIN: u16 = 200;
const RES_MAX: u16 = 3200;

/// PMW3610 configuration
#[derive(Clone)]
pub struct Pmw3610Config {
    /// CPI resolution (200-3200, step 200). Set to -1 to use default.
    pub res_cpi: i16,
    /// Invert X axis
    pub invert_x: bool,
    /// Invert Y axis
    pub invert_y: bool,
    /// Swap X and Y axes
    pub swap_xy: bool,
    /// Force awake mode (disable power saving)
    pub force_awake: bool,
    /// Enable smart mode for better tracking on shiny surfaces
    pub smart_mode: bool,
}

impl Default for Pmw3610Config {
    fn default() -> Self {
        Self {
            res_cpi: -1,
            invert_x: false,
            invert_y: false,
            swap_xy: false,
            force_awake: false,
            smart_mode: false,
        }
    }
}

/// PMW3610 error types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Pmw3610Error {
    /// SPI communication error
    Spi,
    /// Invalid product ID detected
    InvalidProductId(u8),
    /// Initialization failed
    InitFailed,
    /// Invalid CPI value
    InvalidCpi,
}

/// Register snapshot used to reject motion after a PMW3610 power/SPI glitch.
///
/// A hot-swappable module can disappear without producing an SPI transport
/// error: an undriven SDIO line commonly reads as `0xff`.  Keeping both the
/// observed and expected values lets the keyboard log the state that caused a
/// recovery before the sensor is reset and configured again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pmw3610Health {
    pub product_id: u8,
    pub observation1: u8,
    pub performance: u8,
    pub res_step: u8,
    pub smart_mode: u8,
    pub expected_performance: u8,
    pub expected_res_step: u8,
    pub expected_smart_mode: u8,
}

impl Pmw3610Health {
    pub fn is_valid(&self) -> bool {
        self.product_id == PRODUCT_ID_PMW3610
            && self.observation1 & OBSERVATION1_INIT_MASK == OBSERVATION1_INIT_MASK
            && self.performance == self.expected_performance
            && self.res_step == self.expected_res_step
            && self.smart_mode & SMART_MODE_DISABLE == self.expected_smart_mode
    }
}

impl From<Pmw3610Error> for PointingDriverError {
    fn from(err: Pmw3610Error) -> Self {
        match err {
            Pmw3610Error::Spi => PointingDriverError::Spi,
            Pmw3610Error::InvalidProductId(id) => PointingDriverError::InvalidProductId(id),
            Pmw3610Error::InitFailed => PointingDriverError::InitFailed,
            Pmw3610Error::InvalidCpi => PointingDriverError::InvalidCpi,
        }
    }
}

/// PMW3610 driver using embedded-hal SPI traits
pub struct Pmw3610<SPI: SpiBus, CS: OutputPin, MOTION: InputPin + Wait> {
    id: u8,
    spi: SPI,
    cs: CS,
    motion_gpio: Option<MOTION>,
    config: Pmw3610Config,
    smart_flag: bool,
}

impl<SPI: SpiBus, CS: OutputPin, MOTION: InputPin + Wait> Pmw3610<SPI, CS, MOTION> {
    /// Create a new PMW3610 driver instance
    pub fn new(id: u8, spi: SPI, cs: CS, motion_gpio: Option<MOTION>, config: Pmw3610Config) -> Self {
        Self {
            id,
            spi,
            cs,
            motion_gpio,
            config,
            smart_flag: false,
        }
    }

    /// Check that a connected sensor is still present and initialized.
    ///
    /// This is intentionally lightweight so hot-swappable devices can verify
    /// their PMW3610 periodically without resetting a healthy sensor.
    pub async fn is_configured(&mut self) -> bool {
        let Ok(product_id) = self.read_reg(PMW3610_PROD_ID).await else {
            return false;
        };
        let Ok(performance) = self.read_reg(PMW3610_PERFORMANCE).await else {
            return false;
        };
        configured_registers_valid(product_id, performance)
    }

    /// Read every register that can change motion orientation or tracking
    /// mode.  The caller must suppress motion until this snapshot is valid.
    pub async fn configuration_health(&mut self, cpi: u16) -> Result<Pmw3610Health, PointingDriverError> {
        if !(RES_MIN..=RES_MAX).contains(&cpi) || !cpi.is_multiple_of(RES_STEP) {
            return Err(PointingDriverError::InvalidCpi);
        }

        // These registers live on page 0. If the sensor was disconnected in
        // the middle of a page switch, at least the product ID will fail and
        // force a full reinitialization.
        let product_id = self.read_reg(PMW3610_PROD_ID).await?;
        self.spi_clk_on().await?;
        let register_result = async {
            let observation1 = self.read_reg(PMW3610_OBSERVATION1).await?;
            let performance = self.read_reg(PMW3610_PERFORMANCE).await?;
            let smart_mode = self.read_reg(PMW3610_SMART_MODE).await?;
            self.write_reg(PMW3610_SPI_PAGE0, SPI_PAGE0_1).await?;
            let res_step = self.read_reg(PMW3610_RES_STEP).await?;
            Ok::<_, Pmw3610Error>((observation1, performance, smart_mode, res_step))
        }
        .await;
        // Always attempt to restore page 0 and release the requested SPI
        // clock, including when the page-1 read itself failed.
        let page_restore_result = self.write_reg(PMW3610_SPI_PAGE1, SPI_PAGE1_0).await;
        let clock_off_result = self.spi_clk_off().await;
        let (observation1, performance, smart_mode, res_step) = register_result?;
        page_restore_result?;
        clock_off_result?;

        let axis_bits = (u8::from(self.config.swap_xy) << RES_STEP_SWAP_XY_BIT)
            | (u8::from(self.config.invert_x) << RES_STEP_INV_X_BIT)
            | (u8::from(self.config.invert_y) << RES_STEP_INV_Y_BIT);
        let expected_res_step = axis_bits | ((cpi / RES_STEP) as u8 & RES_STEP_RES_MASK);
        let expected_performance = PERFORMANCE_INIT
            | if self.config.force_awake {
                PERFORMANCE_FMODE_FORCE_AWAKE
            } else {
                PERFORMANCE_FMODE_NORMAL
            };
        let expected_smart_mode = if self.config.smart_mode {
            if self.smart_flag {
                SMART_MODE_DISABLE
            } else {
                SMART_MODE_ENABLE
            }
        } else {
            SMART_MODE_DISABLE
        };

        Ok(Pmw3610Health {
            product_id,
            observation1,
            performance,
            res_step,
            smart_mode,
            expected_performance,
            expected_res_step,
            expected_smart_mode,
        })
    }

    /// Put the sensor into shutdown. Only `init()` brings it back: its power-up
    /// reset wakes the sensor and restores the configuration.
    pub async fn shutdown(&mut self) -> Result<(), PointingDriverError> {
        self.write_reg(PMW3610_SHUTDOWN, SHUTDOWN_ENABLE).await?;
        Ok(())
    }

    /// Set force awake mode
    async fn set_force_awake(&mut self, enable: bool) -> Result<(), PointingDriverError> {
        let mut val = self.read_reg(PMW3610_PERFORMANCE).await?;
        val &= !PERFORMANCE_FMODE_MASK;
        if enable {
            val |= PERFORMANCE_FMODE_FORCE_AWAKE;
        } else {
            val |= PERFORMANCE_FMODE_NORMAL;
        }

        self.spi_clk_on().await?;
        self.write_reg(PMW3610_PERFORMANCE, val).await?;
        self.spi_clk_off().await?;

        Ok(())
    }

    /// Apply the requested smart-register state explicitly and verify it.
    ///
    /// PMW3610 powers up with smart tracking enabled. Merely skipping the
    /// adaptive shutter logic therefore does not disable the sensor feature;
    /// diagnostic builds need an explicit register write for a valid A/B test.
    async fn configure_smart_mode(&mut self, enable: bool) -> Result<(), PointingDriverError> {
        let expected = if enable { SMART_MODE_ENABLE } else { SMART_MODE_DISABLE };

        self.spi_clk_on().await?;
        self.write_reg(PMW3610_SMART_MODE, expected).await?;
        let readback = self.read_reg(PMW3610_SMART_MODE).await?;
        self.spi_clk_off().await?;

        let verified = (readback & SMART_MODE_DISABLE) == expected;
        #[cfg(feature = "rtt_diag")]
        info!(
            "[PMW_SMART_CFG_V14] smart_enabled={} force_awake={} register={:#04x} verified={}",
            enable, self.config.force_awake, readback, verified
        );
        if !verified {
            error!(
                "PMW3610 smart-mode register mismatch: expected={:#04x}, got={:#04x}",
                expected, readback
            );
            return Err(PointingDriverError::InitFailed);
        }

        self.smart_flag = !enable;
        Ok(())
    }

    async fn read_reg(&mut self, addr: u8) -> Result<u8, Pmw3610Error> {
        let _ = self.cs.set_low();
        Timer::after(Duration::from_nanos(T_NCS_SCLK_NS)).await;

        self.spi.write(&[addr & 0x7f]).await.map_err(|_| Pmw3610Error::Spi)?;

        Timer::after(Duration::from_micros(T_SRAD_US)).await;

        let mut value = [0u8];
        self.spi.read(&mut value).await.map_err(|_| Pmw3610Error::Spi)?;

        Timer::after(Duration::from_nanos(T_SCLK_NCS_R_NS)).await;
        let _ = self.cs.set_high();

        Timer::after(Duration::from_nanos(T_SRX_NS)).await;

        Ok(value[0])
    }

    async fn read_burst(&mut self, addr: u8, data: &mut [u8]) -> Result<(), Pmw3610Error> {
        let _ = self.cs.set_low();
        Timer::after(Duration::from_nanos(T_NCS_SCLK_NS)).await;

        self.spi.write(&[addr & 0x7f]).await.map_err(|_| Pmw3610Error::Spi)?;

        Timer::after(Duration::from_micros(T_SRAD_US)).await;

        self.spi.read(data).await.map_err(|_| Pmw3610Error::Spi)?;

        Timer::after(Duration::from_nanos(T_SCLK_NCS_R_NS)).await;
        let _ = self.cs.set_high();

        Timer::after(Duration::from_nanos(T_BEXIT_NS)).await;

        Ok(())
    }

    async fn write_reg(&mut self, addr: u8, value: u8) -> Result<(), Pmw3610Error> {
        let _ = self.cs.set_low();
        Timer::after(Duration::from_nanos(T_NCS_SCLK_NS)).await;

        self.spi
            .write(&[addr | SPI_WRITE, value])
            .await
            .map_err(|_| Pmw3610Error::Spi)?;

        Timer::after(Duration::from_micros(T_SCLK_NCS_W_US)).await;
        let _ = self.cs.set_high();

        Timer::after(Duration::from_micros(T_SWX_US)).await;

        Ok(())
    }

    async fn spi_clk_on(&mut self) -> Result<(), Pmw3610Error> {
        self.write_reg(PMW3610_SPI_CLK_ON_REQ, SPI_CLOCK_ON_REQ_ON).await?;
        Timer::after(Duration::from_micros(CLOCK_ON_DELAY_US)).await;
        Ok(())
    }

    async fn spi_clk_off(&mut self) -> Result<(), Pmw3610Error> {
        self.write_reg(PMW3610_SPI_CLK_ON_REQ, SPI_CLOCK_ON_REQ_OFF).await
    }

    async fn configure(&mut self) -> Result<(), Pmw3610Error> {
        self.write_reg(PMW3610_POWER_UP_RESET, POWER_UP_RESET_VAL).await?;
        Timer::after(Duration::from_millis(RESET_DELAY_MS)).await;

        let val = self.read_reg(PMW3610_PROD_ID).await?;
        if val != PRODUCT_ID_PMW3610 {
            error!("Invalid product id: {:#02x}", val);
            return Err(Pmw3610Error::InvalidProductId(val));
        }
        info!("PMW3610 detected, product ID: {:#02x}", val);

        self.spi_clk_on().await?;

        self.write_reg(PMW3610_OBSERVATION1, 0).await?;
        Timer::after(Duration::from_millis(INIT_OBSERVATION_DELAY_MS)).await;

        let val = self.read_reg(PMW3610_OBSERVATION1).await?;
        if (val & OBSERVATION1_INIT_MASK) != OBSERVATION1_INIT_MASK {
            error!("Unexpected OBSERVATION1 value: {:#02x}", val);
            return Err(Pmw3610Error::InitFailed);
        }

        for reg in PMW3610_MOTION..=PMW3610_DELTA_XY_H {
            self.read_reg(reg).await?;
        }

        self.write_reg(PMW3610_PERFORMANCE, PERFORMANCE_INIT).await?;
        self.write_reg(PMW3610_RUN_DOWNSHIFT, RUN_DOWNSHIFT_INIT).await?;
        self.write_reg(PMW3610_REST1_RATE, REST1_RATE_INIT).await?;
        self.write_reg(PMW3610_REST1_DOWNSHIFT, REST1_DOWNSHIFT_INIT).await?;

        self.write_reg(PMW3610_SPI_PAGE0, SPI_PAGE0_1).await?;

        let mut res_step_val = self.read_reg(PMW3610_RES_STEP).await?;

        if self.config.swap_xy {
            res_step_val |= 1 << RES_STEP_SWAP_XY_BIT;
        } else {
            res_step_val &= !(1 << RES_STEP_SWAP_XY_BIT);
        }

        if self.config.invert_x {
            res_step_val |= 1 << RES_STEP_INV_X_BIT;
        } else {
            res_step_val &= !(1 << RES_STEP_INV_X_BIT);
        }

        if self.config.invert_y {
            res_step_val |= 1 << RES_STEP_INV_Y_BIT;
        } else {
            res_step_val &= !(1 << RES_STEP_INV_Y_BIT);
        }

        self.write_reg(PMW3610_RES_STEP, res_step_val).await?;
        let res_step_readback = self.read_reg(PMW3610_RES_STEP).await?;
        let axis_mask = (1 << RES_STEP_SWAP_XY_BIT) | (1 << RES_STEP_INV_X_BIT) | (1 << RES_STEP_INV_Y_BIT);
        let axis_verified = res_step_readback & axis_mask == res_step_val & axis_mask;
        #[cfg(feature = "rtt_diag")]
        info!(
            "[PMW_AXIS_HW_V14] swap_xy={} invert_x={} invert_y={} register={:#04x} verified={}",
            self.config.swap_xy, self.config.invert_x, self.config.invert_y, res_step_readback, axis_verified
        );
        if !axis_verified {
            error!(
                "PMW3610 axis register mismatch: expected={:#04x}, got={:#04x}",
                res_step_val & axis_mask,
                res_step_readback & axis_mask
            );
            return Err(Pmw3610Error::InitFailed);
        }
        self.write_reg(PMW3610_SPI_PAGE1, SPI_PAGE1_0).await?;

        self.spi_clk_off().await?;

        if self.config.res_cpi > 0 {
            self.set_resolution(self.config.res_cpi as u16)
                .await
                .map_err(|_| Pmw3610Error::Spi)?;
        }

        self.set_force_awake(self.config.force_awake)
            .await
            .map_err(|_| Pmw3610Error::Spi)?;
        self.configure_smart_mode(self.config.smart_mode)
            .await
            .map_err(|_| Pmw3610Error::InitFailed)?;

        info!("PMW3610 initialized successfully");
        Ok(())
    }

    fn sign_extend(value: u16, bits: usize) -> i16 {
        let sign_bit = 1 << bits;
        if value & sign_bit != 0 {
            (value | !((1 << (bits + 1)) - 1)) as i16
        } else {
            value as i16
        }
    }
}

fn configured_registers_valid(product_id: u8, performance: u8) -> bool {
    product_id == PRODUCT_ID_PMW3610 && performance & !PERFORMANCE_FMODE_MASK == PERFORMANCE_INIT
}

impl<SPI, CS, MOTION> PointingDriver for Pmw3610<SPI, CS, MOTION>
where
    SPI: SpiBus,
    CS: OutputPin,
    MOTION: InputPin + Wait,
{
    type MOTION = MOTION;

    /// Initialize the sensor (public API)
    async fn init(&mut self) -> Result<(), PointingDriverError> {
        let _ = self.cs.set_high();
        Timer::after(Duration::from_millis(1)).await;

        self.configure().await?;
        Ok(())
    }

    /// Read motion data from the sensor
    async fn read_motion(&mut self) -> Result<MotionData, PointingDriverError> {
        // Keep the full optical burst in RTT diagnostics even when smart mode
        // is disabled, so 09C remains directly comparable with 09A/09B.
        let collect_optics = self.config.smart_mode || cfg!(feature = "rtt_diag");
        let burst_data_len = if collect_optics {
            BURST_DATA_LEN_SMART
        } else {
            BURST_DATA_LEN_NORMAL
        };

        let mut burst_data = [0u8; BURST_DATA_LEN_SMART];
        self.read_burst(PMW3610_BURST_READ, &mut burst_data[..burst_data_len])
            .await?;

        let motion_set = (burst_data[BURST_MOTION] & MOTION_STATUS_MOTION) != 0;
        let _squal = if collect_optics { burst_data[BURST_SQUAL] } else { 0 };
        let shutter_val = if collect_optics {
            ((burst_data[BURST_SHUTTER_HI] as u16) << 8) | (burst_data[BURST_SHUTTER_LO] as u16)
        } else {
            0
        };
        #[cfg(feature = "rtt_diag")]
        crate::rtt_diag::record_pmw_optics(
            motion_set,
            _squal,
            shutter_val,
            SHUTTER_SMART_THRESHOLD,
            self.smart_flag,
        );

        if !motion_set {
            #[cfg(feature = "rtt_diag")]
            crate::rtt_diag::record_pmw_raw(
                burst_data[BURST_MOTION],
                burst_data[BURST_DELTA_X_L],
                burst_data[BURST_DELTA_Y_L],
                burst_data[BURST_DELTA_XY_H],
                0,
                0,
            );
            return Ok(MotionData::default());
        }

        let x = ((burst_data[BURST_DELTA_XY_H] as u16) << 4) & 0xf00 | (burst_data[BURST_DELTA_X_L] as u16);
        let y = ((burst_data[BURST_DELTA_XY_H] as u16) << 8) & 0xf00 | (burst_data[BURST_DELTA_Y_L] as u16);

        let dx = Self::sign_extend(x, PMW3610_DATA_SIZE_BITS - 1);
        let dy = Self::sign_extend(y, PMW3610_DATA_SIZE_BITS - 1);

        #[cfg(feature = "rtt_diag")]
        crate::rtt_diag::record_pmw_raw(
            burst_data[BURST_MOTION],
            burst_data[BURST_DELTA_X_L],
            burst_data[BURST_DELTA_Y_L],
            burst_data[BURST_DELTA_XY_H],
            dx,
            dy,
        );

        if self.config.smart_mode {
            if self.smart_flag && shutter_val < SHUTTER_SMART_THRESHOLD {
                self.spi_clk_on().await?;
                self.write_reg(PMW3610_SMART_MODE, SMART_MODE_ENABLE)
                    .await
                    .map_err(|_| PointingDriverError::Spi)?;
                self.spi_clk_off().await?;
                self.smart_flag = false;
                #[cfg(feature = "rtt_diag")]
                crate::rtt_diag::record_pmw_smart_transition(false, _squal, shutter_val);
            } else if !self.smart_flag && shutter_val > SHUTTER_SMART_THRESHOLD {
                self.spi_clk_on().await?;
                self.write_reg(PMW3610_SMART_MODE, SMART_MODE_DISABLE)
                    .await
                    .map_err(|_| PointingDriverError::Spi)?;
                self.spi_clk_off().await?;
                self.smart_flag = true;
                #[cfg(feature = "rtt_diag")]
                crate::rtt_diag::record_pmw_smart_transition(true, _squal, shutter_val);
            }
        }

        Ok(MotionData { dx, dy })
    }

    /// Check if motion is pending (motion GPIO is active low)
    fn motion_pending(&mut self) -> bool {
        match &mut self.motion_gpio {
            Some(gpio) => gpio.is_low().unwrap_or(true),
            None => true,
        }
    }

    fn motion_gpio(&mut self) -> Option<&mut MOTION> {
        self.motion_gpio.as_mut()
    }

    /// Set sensor resolution in CPI (200-3200, step 200)
    async fn set_resolution(&mut self, cpi: u16) -> Result<(), PointingDriverError> {
        if !(RES_MIN..=RES_MAX).contains(&cpi) {
            return Err(PointingDriverError::InvalidCpi);
        }

        self.spi_clk_on().await?;

        self.write_reg(PMW3610_SPI_PAGE0, SPI_PAGE0_1).await?;

        let mut val = self.read_reg(PMW3610_RES_STEP).await?;
        val &= !RES_STEP_RES_MASK;
        val |= (cpi / RES_STEP) as u8;

        self.write_reg(PMW3610_RES_STEP, val).await?;
        self.write_reg(PMW3610_SPI_PAGE1, SPI_PAGE1_0).await?;

        self.spi_clk_off().await?;

        debug!("PMW3610: Resolution set to {} CPI", cpi);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy_snapshot() -> Pmw3610Health {
        Pmw3610Health {
            product_id: PRODUCT_ID_PMW3610,
            observation1: OBSERVATION1_INIT_MASK,
            performance: PERFORMANCE_INIT,
            res_step: 3,
            smart_mode: SMART_MODE_ENABLE,
            expected_performance: PERFORMANCE_INIT,
            expected_res_step: 3,
            expected_smart_mode: SMART_MODE_ENABLE,
        }
    }

    #[test]
    fn configured_signature_accepts_power_modes_and_rejects_reset_sensor() {
        assert!(configured_registers_valid(PRODUCT_ID_PMW3610, PERFORMANCE_INIT));
        assert!(configured_registers_valid(
            PRODUCT_ID_PMW3610,
            PERFORMANCE_INIT | PERFORMANCE_FMODE_FORCE_AWAKE
        ));
        assert!(!configured_registers_valid(0xff, PERFORMANCE_INIT));
        assert!(!configured_registers_valid(PRODUCT_ID_PMW3610, 0));
    }

    #[test]
    fn detailed_health_rejects_every_motion_configuration_mismatch() {
        assert!(healthy_snapshot().is_valid());

        let mut health = healthy_snapshot();
        health.product_id = 0xff;
        assert!(!health.is_valid());

        let mut health = healthy_snapshot();
        health.observation1 = 0;
        assert!(!health.is_valid());

        let mut health = healthy_snapshot();
        health.performance = 0;
        assert!(!health.is_valid());

        let mut health = healthy_snapshot();
        health.res_step |= 1 << RES_STEP_SWAP_XY_BIT;
        assert!(!health.is_valid());

        let mut health = healthy_snapshot();
        health.smart_mode = SMART_MODE_DISABLE;
        assert!(!health.is_valid());
    }
}

impl<SPI, CS, MOTION> PointingDevice<Pmw3610<SPI, CS, MOTION>>
where
    SPI: SpiBus,
    CS: OutputPin,
    MOTION: InputPin + Wait,
{
    const DEFAULT_POLL_INTERVAL_US: u64 = 500;
    const DEFAULT_REPORT_HZ: u16 = 125;

    /// Create a new PMW3610 device
    pub fn new(id: u8, spi: SPI, cs: CS, motion_gpio: Option<MOTION>, sensor_config: Pmw3610Config) -> Self {
        Self::with_poll_interval_and_report_hz(
            id,
            spi,
            cs,
            motion_gpio,
            sensor_config,
            Self::DEFAULT_POLL_INTERVAL_US,
            Self::DEFAULT_REPORT_HZ,
        )
    }

    /// Create a new PMW3610 device with custom report rate (Hz)
    pub fn with_report_hz(
        id: u8,
        spi: SPI,
        cs: CS,
        motion_gpio: Option<MOTION>,
        sensor_config: Pmw3610Config,
        report_hz: u16,
    ) -> Self {
        Self::with_poll_interval_and_report_hz(
            id,
            spi,
            cs,
            motion_gpio,
            sensor_config,
            Self::DEFAULT_POLL_INTERVAL_US,
            report_hz,
        )
    }

    /// Create a new PMW3610 device with custom poll interval
    pub fn with_poll_interval(
        id: u8,
        spi: SPI,
        cs: CS,
        motion_gpio: Option<MOTION>,
        sensor_config: Pmw3610Config,
        poll_interval_us: u64,
    ) -> Self {
        Self::with_poll_interval_and_report_hz(
            id,
            spi,
            cs,
            motion_gpio,
            sensor_config,
            poll_interval_us,
            Self::DEFAULT_REPORT_HZ,
        )
    }

    /// Create a new PMW3610 device with custom poll interval and report rate
    pub fn with_poll_interval_and_report_hz(
        id: u8,
        spi: SPI,
        cs: CS,
        motion_gpio: Option<MOTION>,
        sensor_config: Pmw3610Config,
        poll_interval_us: u64,
        report_hz: u16,
    ) -> Self {
        let report_interval = Duration::from_hz(report_hz as u64);

        // Polling should be more frequent than reporting
        let poll_interval = Duration::from_micros(poll_interval_us).min(report_interval);

        Self {
            id,
            sensor: Pmw3610::new(id, spi, cs, motion_gpio, sensor_config),
            init_state: InitState::Pending,
            poll_interval,
            report_interval,
            last_poll: Instant::MIN,
            last_report: Instant::MIN,
            accumulated_x: 0,
            accumulated_y: 0,
        }
    }
}
