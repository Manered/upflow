use crate::Error;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub theme: Theme,
    pub output_directory: Option<PathBuf>,
    pub use_source_directory: bool,
    pub threads: u16,
    pub open_folder_after_conversion: bool,
    pub keep_logs: bool,
    pub ffmpeg_path: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: Theme::System,
            output_directory: None,
            use_source_directory: true,
            threads: 16,
            open_folder_after_conversion: false,
            keep_logs: false,
            ffmpeg_path: None,
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        let mut settings: Self = fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        if !(1..=256).contains(&settings.threads) {
            settings.threads = 16;
        }
        let has_valid_output_directory = settings
            .output_directory
            .as_ref()
            .is_some_and(|path| path.is_absolute());
        if !has_valid_output_directory {
            settings.output_directory = None;
            settings.use_source_directory = true;
        }
        let has_valid_ffmpeg_path = settings
            .ffmpeg_path
            .as_ref()
            .is_some_and(|path| path.is_absolute());
        if !has_valid_ffmpeg_path {
            settings.ffmpeg_path = None;
        }
        settings
    }

    pub fn save(&self, path: &Path) -> Result<(), Error> {
        if !(1..=256).contains(&self.threads) {
            return Err(Error::new(
                "settings",
                "Enter a thread count from 1 to 256.",
                "",
            ));
        }
        let has_valid_output_directory = self.use_source_directory
            || self
                .output_directory
                .as_ref()
                .is_some_and(|path| path.is_absolute() && path.is_dir());
        if !has_valid_output_directory {
            return Err(Error::new(
                "outputDirectory",
                "Choose an existing output folder.",
                "",
            ));
        }
        let has_valid_ffmpeg_path = self
            .ffmpeg_path
            .as_ref()
            .is_none_or(|path| path.is_absolute() && path.is_file());
        if !has_valid_ffmpeg_path {
            return Err(Error::new(
                "missingTool",
                "Choose a valid FFmpeg executable.",
                "",
            ));
        }
        self.write(path)
    }

    fn write(&self, path: &Path) -> Result<(), Error> {
        let directory = path.parent().expect("settings path has a parent");
        let write = || -> std::io::Result<()> {
            fs::create_dir_all(directory)?;
            let mut temp = tempfile::NamedTempFile::new_in(directory)?;
            serde_json::to_writer_pretty(temp.as_file_mut(), self)?;
            temp.write_all(b"\n")?;
            temp.as_file().sync_all()?;
            temp.persist(path).map_err(|e| e.error)?;
            Ok(())
        };
        write().map_err(|e| Error::io(e, "settings", "Could not save settings."))
    }
}
