// Copyright (C) 2016-2017 Sebastian Dröge <sebastian@centricular.com>
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::u64;
use std::io::Read;
use url::Url;
use reqwest::{Client, Response};
use reqwest::header::{ContentLength, ContentRange, ContentRangeSpec, Range, ByteRangeSpec,
                      AcceptRanges, RangeUnit};

use gst_plugin::error::*;
use gst_plugin::source::*;
use gst_plugin::buffer::*;
use gst_plugin::utils::*;
use gst_plugin::log::*;

use slog::Logger;

#[derive(Debug)]
enum StreamingState {
    Stopped,
    Started {
        uri: Url,
        response: Response,
        seekable: bool,
        position: u64,
        size: Option<u64>,
        start: u64,
        stop: Option<u64>,
    },
}

#[derive(Debug)]
pub struct HttpSrc {
    streaming_state: StreamingState,
    logger: Logger,
    client: Client,
}

impl HttpSrc {
    pub fn new(element: Element) -> HttpSrc {
        HttpSrc {
            streaming_state: StreamingState::Stopped,
            logger: Logger::root(GstDebugDrain::new(Some(&element),
                                                    "rshttpsink",
                                                    0,
                                                    "Rust http sink"),
                                 o!()),
            client: Client::new().unwrap(),
        }
    }

    pub fn new_boxed(element: Element) -> Box<Source> {
        Box::new(HttpSrc::new(element))
    }

    fn do_request(&self,
                  uri: Url,
                  start: u64,
                  stop: Option<u64>)
                  -> Result<StreamingState, ErrorMessage> {
        let mut req = self.client.get(uri.clone());

        match (start != 0, stop) {
            (false, None) => (),
            (true, None) => req = req.header(Range::Bytes(vec![ByteRangeSpec::AllFrom(start)])),
            (_, Some(stop)) => {
                req = req.header(Range::Bytes(vec![ByteRangeSpec::FromTo(start, stop - 1)]))
            }
        }

        debug!(self.logger, "Doing new request {:?}", req);

        let response =
            try!(req.send()
                     .or_else(|err| {
                                  error!(self.logger, "Request failed: {:?}", err);
                                  Err(error_msg!(SourceError::ReadFailed,
                                                 ["Failed to fetch {}: {}", uri, err.to_string()]))
                              }));

        if !response.status().is_success() {
            error!(self.logger, "Request status failed: {:?}", response);
            return Err(error_msg!(SourceError::ReadFailed,
                                  ["Failed to fetch {}: {}", uri, response.status()]));
        }

        let size = response
            .headers()
            .get()
            .map(|&ContentLength(cl)| cl + start);

        let accept_byte_ranges = if let Some(&AcceptRanges(ref ranges)) =
            response.headers().get() {
            ranges.iter().any(|u| *u == RangeUnit::Bytes)
        } else {
            false
        };

        let seekable = size.is_some() && accept_byte_ranges;

        let position = if let Some(&ContentRange(ContentRangeSpec::Bytes {
                                                     range: Some((range_start, _)), ..
                                                 })) = response.headers().get() {
            range_start
        } else {
            start
        };

        if position != start {
            return Err(error_msg!(SourceError::SeekFailed,
                                  ["Failed to seek to {}: Got {}", start, position]));
        }

        debug!(self.logger, "Request successful: {:?}", response);

        Ok(StreamingState::Started {
               uri: uri,
               response: response,
               seekable: seekable,
               position: 0,
               size: size,
               start: start,
               stop: stop,
           })
    }
}

fn validate_uri(uri: &Url) -> Result<(), UriError> {
    if uri.scheme() != "http" && uri.scheme() != "https" {
        return Err(UriError::new(UriErrorKind::UnsupportedProtocol,
                                 Some(format!("Unsupported URI '{}'", uri.as_str()))));
    }

    Ok(())
}

impl Source for HttpSrc {
    fn uri_validator(&self) -> Box<UriValidator> {
        Box::new(validate_uri)
    }

    fn is_seekable(&self) -> bool {
        match self.streaming_state {
            StreamingState::Started { seekable, .. } => seekable,
            _ => false,
        }
    }

    fn get_size(&self) -> Option<u64> {
        match self.streaming_state {
            StreamingState::Started { size, .. } => size,
            _ => None,
        }
    }

    fn start(&mut self, uri: Url) -> Result<(), ErrorMessage> {
        self.streaming_state = StreamingState::Stopped;
        self.streaming_state = try!(self.do_request(uri, 0, None));

        Ok(())
    }

    fn stop(&mut self) -> Result<(), ErrorMessage> {
        self.streaming_state = StreamingState::Stopped;

        Ok(())
    }

    fn seek(&mut self, start: u64, stop: Option<u64>) -> Result<(), ErrorMessage> {
        let (position, old_stop, uri) = match self.streaming_state {
            StreamingState::Started {
                position,
                stop,
                ref uri,
                ..
            } => (position, stop, uri.clone()),
            StreamingState::Stopped => {
                return Err(error_msg!(SourceError::Failure, ["Not started yet"]));
            }
        };

        if position == start && old_stop == stop {
            return Ok(());
        }

        self.streaming_state = StreamingState::Stopped;
        self.streaming_state = try!(self.do_request(uri, start, stop));

        Ok(())
    }

    fn fill(&mut self, offset: u64, _: u32, buffer: &mut Buffer) -> Result<(), FlowError> {
        let logger = self.logger.clone();

        let (response, position) = match self.streaming_state {
            StreamingState::Started {
                ref mut response,
                ref mut position,
                ..
            } => (response, position),
            StreamingState::Stopped => {
                return Err(FlowError::Error(error_msg!(SourceError::Failure, ["Not started yet"])));
            }
        };

        if *position != offset {
            return Err(FlowError::Error(error_msg!(SourceError::SeekFailed,
                                                   ["Got unexpected offset {}, expected {}",
                                                    offset,
                                                    position])));
        }

        let size = {
            let mut map = match buffer.map_readwrite() {
                None => {
                    return Err(FlowError::Error(error_msg!(SourceError::Failure,
                                                           ["Failed to map buffer"])));
                }
                Some(map) => map,
            };

            let data = map.as_mut_slice();

            try!(response
                     .read(data)
                     .or_else(|err| {
                                  error!(logger, "Failed to read: {:?}", err);
                                  Err(FlowError::Error(error_msg!(SourceError::ReadFailed,
                                                                  ["Failed to read at {}: {}",
                                                                   offset,
                                                                   err.to_string()])))
                              }))
        };

        if size == 0 {
            return Err(FlowError::Eos);
        }

        *position += size as u64;

        buffer.set_size(size);

        Ok(())
    }
}
