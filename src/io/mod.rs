use std::fs::File;
use std::path::Path;

pub struct MappedFile {
    mmap: Option<memmap2::Mmap>,
}

impl MappedFile {
    pub fn open(path: &Path) -> crate::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        if len == 0 {
            return Ok(MappedFile { mmap: None });
        }
        let mmap = unsafe { memmap2::MmapOptions::new().map(&file)? };
        Ok(MappedFile { mmap: Some(mmap) })
    }

    pub fn as_bytes(&self) -> &[u8] {
        match &self.mmap {
            Some(m) => &m[..],
            None => &[],
        }
    }
}
