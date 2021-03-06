// Copyright (C) 2016-2017 Sebastian Dröge <sebastian@centricular.com>
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use buffer::*;
use miniobject::*;
use log::*;
use std::collections::VecDeque;
use std::cmp;
use slog::Logger;

lazy_static! {
    static ref LOGGER: Logger = {
        Logger::root(GstDebugDrain::new(None,
                                        "rsadapter",
                                        0,
                                        "Rust buffer adapter"),
                    o!())
    };
}

#[derive(Debug)]
pub struct Adapter {
    deque: VecDeque<ReadMappedBuffer>,
    size: usize,
    skip: usize,
    scratch: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AdapterError {
    NotEnoughData,
}

impl Adapter {
    pub fn new() -> Adapter {
        Adapter {
            deque: VecDeque::new(),
            size: 0,
            skip: 0,
            scratch: Vec::new(),
        }
    }

    pub fn push(&mut self, buffer: GstRc<Buffer>) {
        let size = buffer.get_size();

        self.size += size;
        trace!(LOGGER,
               "Storing {:?} of size {}, now have size {}",
               buffer,
               size,
               self.size);
        self.deque
            .push_back(Buffer::into_read_mapped_buffer(buffer).unwrap());
    }

    pub fn clear(&mut self) {
        self.deque.clear();
        self.size = 0;
        self.skip = 0;
        self.scratch.clear();
        trace!(LOGGER, "Cleared adapter");
    }

    pub fn get_available(&self) -> usize {
        self.size
    }

    fn copy_data(deque: &VecDeque<ReadMappedBuffer>, skip: usize, data: &mut [u8], size: usize) {
        let mut skip = skip;
        let mut left = size;
        let mut idx = 0;

        trace!(LOGGER, "Copying {} bytes", size);

        for item in deque {
            let data_item = item.as_slice();

            let to_copy = cmp::min(left, data_item.len() - skip);
            trace!(LOGGER,
                   "Copying {} bytes from {:?}, {} more to go",
                   to_copy,
                   item,
                   left - to_copy);

            data[idx..idx + to_copy].copy_from_slice(&data_item[skip..skip + to_copy]);
            skip = 0;
            idx += to_copy;
            left -= to_copy;
            if left == 0 {
                break;
            }
        }
        assert_eq!(left, 0);
    }

    pub fn peek_into(&self, data: &mut [u8]) -> Result<(), AdapterError> {
        let size = data.len();

        if self.size < size {
            debug!(LOGGER,
                   "Peeking {} bytes into, not enough data: have {}",
                   size,
                   self.size);
            return Err(AdapterError::NotEnoughData);
        }

        trace!(LOGGER, "Peeking {} bytes into", size);
        if size == 0 {
            return Ok(());
        }

        Self::copy_data(&self.deque, self.skip, data, size);
        Ok(())
    }

    pub fn peek(&mut self, size: usize) -> Result<&[u8], AdapterError> {
        if self.size < size {
            debug!(LOGGER,
                   "Peeking {} bytes, not enough data: have {}",
                   size,
                   self.size);
            return Err(AdapterError::NotEnoughData);
        }

        if size == 0 {
            return Ok(&[]);
        }

        if let Some(front) = self.deque.front() {
            trace!(LOGGER, "Peeking {} bytes, subbuffer of first", size);
            if front.get_size() - self.skip >= size {
                return Ok(&front.as_slice()[self.skip..self.skip + size]);
            }
        }

        trace!(LOGGER, "Peeking {} bytes, copy to scratch", size);

        self.scratch.truncate(0);
        self.scratch.reserve(size);
        {
            let data = self.scratch.as_mut_slice();
            Self::copy_data(&self.deque, self.skip, data, size);
        }

        Ok(self.scratch.as_slice())
    }

    pub fn get_buffer(&mut self, size: usize) -> Result<GstRc<Buffer>, AdapterError> {
        if self.size < size {
            debug!(LOGGER,
                   "Get buffer of {} bytes, not enough data: have {}",
                   size,
                   self.size);
            return Err(AdapterError::NotEnoughData);
        }

        if size == 0 {
            return Ok(Buffer::new());
        }

        let sub = self.deque
            .front()
            .and_then(|front| if front.get_size() - self.skip >= size {
                          trace!(LOGGER, "Get buffer of {} bytes, subbuffer of first", size);
                          let new = front
                              .get_buffer()
                              .copy_region(self.skip, Some(size))
                              .unwrap();
                          Some(new)
                      } else {
                          None
                      });

        if let Some(s) = sub {
            self.flush(size).unwrap();
            return Ok(s);
        }

        trace!(LOGGER, "Get buffer of {} bytes, copy into new buffer", size);
        let mut new = Buffer::new_with_size(size).unwrap();
        {
            let mut map = new.get_mut().unwrap().map_readwrite().unwrap();
            let data = map.as_mut_slice();
            Self::copy_data(&self.deque, self.skip, data, size);
        }
        self.flush(size).unwrap();
        Ok(new)
    }

    pub fn flush(&mut self, size: usize) -> Result<(), AdapterError> {
        if self.size < size {
            debug!(LOGGER,
                   "Flush {} bytes, not enough data: have {}",
                   size,
                   self.size);
            return Err(AdapterError::NotEnoughData);
        }

        if size == 0 {
            return Ok(());
        }

        trace!(LOGGER, "Flushing {} bytes, have {}", size, self.size);

        let mut left = size;
        while left > 0 {
            let front_size = self.deque.front().unwrap().get_size() - self.skip;

            if front_size <= left {
                trace!(LOGGER,
                       "Flushing whole {:?}, {} more to go",
                       self.deque.front(),
                       left - front_size);
                self.deque.pop_front();
                self.size -= front_size;
                self.skip = 0;
                left -= front_size;
            } else {
                trace!(LOGGER,
                       "Flushing partial {:?}, {} more left",
                       self.deque.front(),
                       front_size - left);
                self.skip += left;
                self.size -= left;
                left = 0;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;
    use gst;

    fn init() {
        unsafe {
            gst::gst_init(ptr::null_mut(), ptr::null_mut());
        }
    }

    #[test]
    fn test_push_get() {
        init();

        let mut a = Adapter::new();

        a.push(Buffer::new_with_size(100).unwrap());
        assert_eq!(a.get_available(), 100);
        a.push(Buffer::new_with_size(20).unwrap());
        assert_eq!(a.get_available(), 120);

        let b = a.get_buffer(20).unwrap();
        assert_eq!(a.get_available(), 100);
        assert_eq!(b.get_size(), 20);
        let b = a.get_buffer(90).unwrap();
        assert_eq!(a.get_available(), 10);
        assert_eq!(b.get_size(), 90);

        a.push(Buffer::new_with_size(20).unwrap());
        assert_eq!(a.get_available(), 30);

        let b = a.get_buffer(20).unwrap();
        assert_eq!(a.get_available(), 10);
        assert_eq!(b.get_size(), 20);
        let b = a.get_buffer(10).unwrap();
        assert_eq!(a.get_available(), 0);
        assert_eq!(b.get_size(), 10);

        let b = a.get_buffer(1);
        assert_eq!(b.err().unwrap(), AdapterError::NotEnoughData);
    }
}
