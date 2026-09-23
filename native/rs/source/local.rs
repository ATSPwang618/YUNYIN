//! Local file source — the only source the shipping player uses today.
//!
//! This is the reference implementation of the seam: the five library decoders
//! and `ym4a.c` all read through a path right now (§29), and Phase 1 swaps that
//! path for a `yp_io` callback pair backed by exactly this object.
#![allow(dead_code)]

use super::{AudioSource, SourceError, SourceKind};
use alloc::string::String;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

pub struct LocalFileSource {
    file: File,
    size: u64,
    pos: u64,
    path: String,
    error: Option<SourceError>,
}

impl LocalFileSource {
    pub fn open(path: &str) -> Result<Self, SourceError> {
        let file = File::open(path).map_err(|e| SourceError::Io(e.to_string()))?;
        let size = file
            .metadata()
            .map_err(|e| SourceError::Io(e.to_string()))?
            .len();
        Ok(Self {
            file,
            size,
            pos: 0,
            path: String::from(path),
            error: None,
        })
    }

    pub fn path(&self) -> &str {
        &self.path
    }
}

impl AudioSource for LocalFileSource {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, SourceError> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.pos >= self.size {
            return Ok(0); /* real EOF */
        }
        /* Never ask the card for more than is left: a short read here would be
         * indistinguishable from EOF to some decoders. */
        let want = buf.len().min((self.size - self.pos) as usize);
        match self.file.read(&mut buf[..want]) {
            Ok(0) => Ok(0),
            Ok(n) => {
                self.pos += n as u64;
                Ok(n)
            }
            Err(e) => {
                let err = SourceError::Io(e.to_string());
                self.error = Some(err.clone());
                Err(err)
            }
        }
    }

    fn seek(&mut self, pos: u64) -> Result<(), SourceError> {
        let target = pos.min(self.size);
        self.file
            .seek(SeekFrom::Start(target))
            .map_err(|e| {
                let err = SourceError::Io(e.to_string());
                self.error = Some(err.clone());
                err
            })?;
        self.pos = target;
        Ok(())
    }

    fn tell(&self) -> u64 {
        self.pos
    }

    fn size(&self) -> Option<u64> {
        Some(self.size)
    }

    fn available(&self) -> usize {
        self.size.saturating_sub(self.pos) as usize
    }

    fn is_eof(&self) -> bool {
        self.pos >= self.size
    }

    fn error(&self) -> Option<SourceError> {
        self.error.clone()
    }

    fn kind(&self) -> SourceKind {
        SourceKind::LocalFile
    }
}
