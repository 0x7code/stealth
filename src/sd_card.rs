//! MicroSD storage for the ESP32-P4X-EYE.
//!
//! The mounted card is exposed through ESP-IDF's virtual filesystem at `/sdcard`,
//! so normal Rust file APIs work after [`SdCard::mount`] succeeds.

use std::{
    fs::{self, File, OpenOptions},
    io::{ErrorKind, Write},
};

use anyhow::Context;
use esp_idf_hal::{
    gpio::{Gpio46, Output, PinDriver},
    ldo::{Adjustable, LDO4},
    sd::mmc::SDMMC0,
};
use esp_idf_svc::sys;

const MOUNT_PATH: &[u8] = b"/sdcard\0";
const LOG_PATH: &str = "/sdcard/stealth.log";
// ESP-IDF's default FAT configuration on this board uses 8.3 filenames. `IMG00000.JPG` keeps
// both the base name and extension within that limit.
const CAPTURE_PATH_PREFIX: &str = "/sdcard/IMG";
const MAX_CAPTURE_FILES: u32 = 100_000;

// The P4X-EYE connects its MicroSD socket to SDMMC slot 0's native pins.
const SDMMC_CLK: i32 = 43;
const SDMMC_CMD: i32 = 44;
const SDMMC_D0: i32 = 39;
const SDMMC_D1: i32 = 40;
const SDMMC_D2: i32 = 41;
const SDMMC_D3: i32 = 42;
const SDMMC_SLOT_0: i32 = 0;
const SDMMC_NO_PIN: i32 = -1;

// These values are the bit flags used by ESP-IDF's SDMMC_HOST_DEFAULT macro.
const SDMMC_HOST_FLAGS: u32 = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 4) | (1 << 5);

#[repr(C)]
struct SdPowerLdoConfig {
    ldo_chan_id: i32,
}

unsafe extern "C" {
    fn sd_pwr_ctrl_new_on_chip_ldo(
        config: *const SdPowerLdoConfig,
        power: *mut sys::sd_pwr_ctrl_handle_t,
    ) -> sys::esp_err_t;
    fn sd_pwr_ctrl_del_on_chip_ldo(power: sys::sd_pwr_ctrl_handle_t) -> sys::esp_err_t;
}

/// P4X-EYE peripherals used exclusively by the MicroSD card.
///
/// `board::P4xEye` assembles this from the board-wide peripheral set.
pub struct SdCardHardware {
    pub(crate) host: SDMMC0<'static>,
    pub(crate) ldo4: LDO4<'static, Adjustable>,
    pub(crate) card_enable_pin: Gpio46<'static>,
}

/// The P4X-EYE MicroSD controller.
///
/// It owns the hardware whether or not a card is currently mounted, so a card inserted after
/// boot can be mounted later without restarting the camera.
pub struct SdCard {
    mounted: Option<MountedCard>,
    _host: SDMMC0<'static>,
    _ldo4: LDO4<'static, Adjustable>,
    _card_enable: PinDriver<'static, Output>,
}

struct MountedCard {
    card: *mut sys::sdmmc_card_t,
    power: sys::sd_pwr_ctrl_handle_t,
}

impl SdCard {
    /// Configure the card power and retain the SDMMC resources for future mount attempts.
    pub fn new(hardware: SdCardHardware) -> anyhow::Result<Self> {
        let SdCardHardware {
            host,
            ldo4,
            card_enable_pin,
        } = hardware;

        // SD_EN is active-low on this board.
        let mut card_enable = PinDriver::output(card_enable_pin)?;
        card_enable.set_low()?;

        Ok(Self {
            mounted: None,
            _host: host,
            _ldo4: ldo4,
            _card_enable: card_enable,
        })
    }

    /// Mount the card at `/sdcard` if it is present and not already mounted.
    ///
    /// Mounting never formats the card. Format it as FAT32 on a computer if mounting fails.
    pub fn mount(&mut self) -> anyhow::Result<()> {
        if self.mounted.is_some() {
            return Ok(());
        }

        // The mount owns this ESP-IDF power-controller handle until unmount.
        let mut power = std::ptr::null_mut();
        sys::EspError::convert(unsafe {
            sd_pwr_ctrl_new_on_chip_ldo(&SdPowerLdoConfig { ldo_chan_id: 4 }, &mut power)
        })
        .context("failed to configure LDO4 for MicroSD I/O")?;

        let host_config = sdmmc_host_config(power);
        let slot_config = sdmmc_slot_config();
        let mount_config = sys::esp_vfs_fat_sdmmc_mount_config_t {
            format_if_mount_failed: false,
            max_files: 5,
            // This only matters if formatting is explicitly enabled. It is a useful size
            // for the future sequential image and video files this module will write.
            allocation_unit_size: 64 * 1024,
            ..Default::default()
        };
        let mut card = std::ptr::null_mut();

        let mount_result = sys::EspError::convert(unsafe {
            sys::esp_vfs_fat_sdmmc_mount(
                MOUNT_PATH.as_ptr().cast(),
                &host_config,
                (&slot_config as *const sys::sdmmc_slot_config_t).cast(),
                &mount_config,
                &mut card,
            )
        })
        .context("failed to mount MicroSD card at /sdcard");

        if let Err(error) = mount_result {
            let _ = unsafe { sd_pwr_ctrl_del_on_chip_ldo(power) };
            return Err(error);
        }

        self.mounted = Some(MountedCard { card, power });
        Ok(())
    }

    /// Append one line to `/sdcard/stealth.log` and immediately flush it to the card.
    pub fn append_log(&self, message: &str) -> anyhow::Result<()> {
        self.require_mount()?;
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(LOG_PATH)
            .context("failed to open /sdcard/stealth.log")?;

        writeln!(log, "{message}").context("failed to append to /sdcard/stealth.log")?;
        log.sync_data()
            .context("failed to flush /sdcard/stealth.log")?;

        Ok(())
    }

    /// Read the complete UTF-8 log file. This is mainly useful to verify the first write.
    pub fn read_log(&self) -> anyhow::Result<String> {
        self.require_mount()?;
        fs::read_to_string(LOG_PATH).context("failed to read /sdcard/stealth.log")
    }

    /// Save one JPEG bitstream as the first free `/sdcard/IMG00000.JPG` capture.
    pub fn save_jpeg(&mut self, jpeg: &[u8]) -> anyhow::Result<String> {
        self.mount().context("MicroSD is unavailable")?;
        let (path, mut file) = create_capture_file("JPG")?;
        let write_result = file
            .write_all(jpeg)
            .context("failed to write JPEG capture to MicroSD")
            .and_then(|()| {
                file.sync_data()
                    .context("failed to flush JPEG capture to MicroSD")
            });
        drop(file);

        if let Err(error) = write_result {
            // Do not leave a file that looks like a valid capture but contains only a prefix.
            let _ = fs::remove_file(&path);
            return Err(error);
        }

        Ok(path)
    }

    fn require_mount(&self) -> anyhow::Result<()> {
        if self.mounted.is_none() {
            anyhow::bail!("MicroSD is not mounted")
        }
        Ok(())
    }
}

impl Drop for SdCard {
    fn drop(&mut self) {
        let Some(mounted) = self.mounted.take() else {
            return;
        };
        if !mounted.card.is_null() {
            // Drop cannot return an error. Normal operation keeps this object alive until reset.
            let _ = unsafe {
                sys::esp_vfs_fat_sdcard_unmount(MOUNT_PATH.as_ptr().cast(), mounted.card)
            };
        }
        if !mounted.power.is_null() {
            let _ = unsafe { sd_pwr_ctrl_del_on_chip_ldo(mounted.power) };
        }
    }
}

fn sdmmc_host_config(power: sys::sd_pwr_ctrl_handle_t) -> sys::sdmmc_host_t {
    sys::sdmmc_host_t {
        flags: SDMMC_HOST_FLAGS,
        slot: SDMMC_SLOT_0,
        max_freq_khz: sys::SDMMC_FREQ_HIGHSPEED as i32,
        io_voltage: 3.3,
        driver_strength: sys::sdmmc_driver_strength_t_SDMMC_DRIVER_STRENGTH_B,
        current_limit: sys::sdmmc_current_limit_t_SDMMC_CURRENT_LIMIT_200MA,
        init: Some(sys::sdmmc_host_init),
        set_bus_width: Some(sys::sdmmc_host_set_bus_width),
        get_bus_width: Some(sys::sdmmc_host_get_slot_width),
        set_bus_ddr_mode: Some(sys::sdmmc_host_set_bus_ddr_mode),
        set_card_clk: Some(sys::sdmmc_host_set_card_clk),
        set_cclk_always_on: Some(sys::sdmmc_host_set_cclk_always_on),
        do_transaction: Some(sys::sdmmc_host_do_transaction),
        __bindgen_anon_1: sys::sdmmc_host_t__bindgen_ty_1 {
            deinit_p: Some(sys::sdmmc_host_deinit_slot),
        },
        io_int_enable: Some(sys::sdmmc_host_io_int_enable),
        io_int_wait: Some(sys::sdmmc_host_io_int_wait),
        command_timeout_ms: 0,
        get_real_freq: Some(sys::sdmmc_host_get_real_freq),
        input_delay_phase: sys::sdmmc_delay_phase_t_SDMMC_DELAY_PHASE_0,
        set_input_delay: Some(sys::sdmmc_host_set_input_delay),
        dma_aligned_buffer: std::ptr::null_mut(),
        pwr_ctrl_handle: power,
        get_dma_info: Some(sys::sdmmc_host_get_dma_info),
        check_buffer_alignment: Some(sys::sdmmc_host_check_buffer_alignment),
        is_slot_set_to_uhs1: Some(sys::sdmmc_host_is_slot_set_to_uhs1),
    }
}

fn sdmmc_slot_config() -> sys::sdmmc_slot_config_t {
    sys::sdmmc_slot_config_t {
        clk: SDMMC_CLK,
        cmd: SDMMC_CMD,
        d0: SDMMC_D0,
        d1: SDMMC_D1,
        d2: SDMMC_D2,
        d3: SDMMC_D3,
        d4: SDMMC_NO_PIN,
        d5: SDMMC_NO_PIN,
        d6: SDMMC_NO_PIN,
        d7: SDMMC_NO_PIN,
        __bindgen_anon_1: sys::sdmmc_slot_config_t__bindgen_ty_1 { cd: SDMMC_NO_PIN },
        __bindgen_anon_2: sys::sdmmc_slot_config_t__bindgen_ty_2 { wp: SDMMC_NO_PIN },
        width: 4,
        flags: 0,
    }
}

fn create_capture_file(extension: &str) -> anyhow::Result<(String, File)> {
    for index in 0..MAX_CAPTURE_FILES {
        let path = format!("{CAPTURE_PATH_PREFIX}{index:05}.{extension}");
        match fs::metadata(&path) {
            Ok(_) => continue,
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).context("failed to inspect camera capture on MicroSD");
            }
        }

        // ESP-IDF's FAT VFS rejects `O_EXCL`, which Rust uses for `create_new`. Capture runs
        // from this one task, so checking first and then using the widely supported `O_CREAT`
        // flag is sufficient here.
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .context("failed to create camera capture on MicroSD")?;
        return Ok((path, file));
    }

    anyhow::bail!("no free camera-capture filenames remain on MicroSD")
}
