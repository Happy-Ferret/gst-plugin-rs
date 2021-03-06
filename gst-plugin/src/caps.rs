// Copyright (C) 2016-2017 Sebastian Dröge <sebastian@centricular.com>
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::ffi::CString;
use std::ffi::CStr;
use std::fmt;
use value::*;
use miniobject::*;
use structure::*;

use glib;
use gst;

#[repr(C)]
pub struct Caps(gst::GstCaps);

unsafe impl MiniObject for Caps {
    type PtrType = gst::GstCaps;
}

impl Caps {
    pub fn new_empty() -> GstRc<Self> {
        unsafe { GstRc::from_owned_ptr(gst::gst_caps_new_empty()) }
    }

    pub fn new_any() -> GstRc<Self> {
        unsafe { GstRc::from_owned_ptr(gst::gst_caps_new_any()) }
    }

    pub fn new_simple(name: &str, values: &[(&str, Value)]) -> GstRc<Self> {
        let mut caps = Caps::new_empty();

        let name_cstr = CString::new(name).unwrap();
        let structure = unsafe { gst::gst_structure_new_empty(name_cstr.as_ptr()) };

        unsafe {
            gst::gst_caps_append_structure(caps.as_mut_ptr(), structure);
        }

        caps.get_mut().unwrap().set_simple(values);

        caps
    }

    pub fn from_string(value: &str) -> Option<GstRc<Self>> {
        let value_cstr = CString::new(value).unwrap();

        unsafe {
            let caps_ptr = gst::gst_caps_from_string(value_cstr.as_ptr());

            if caps_ptr.is_null() {
                None
            } else {
                Some(GstRc::from_owned_ptr(caps_ptr))
            }
        }
    }

    pub fn set_simple(&mut self, values: &[(&str, Value)]) {
        for value in values {
            let name_cstr = CString::new(value.0).unwrap();
            unsafe {
                let gvalue = value.1.as_ptr();
                gst::gst_caps_set_value(self.as_mut_ptr(), name_cstr.as_ptr(), gvalue);
            }
        }
    }

    pub fn to_string(&self) -> String {
        unsafe {
            let ptr = gst::gst_caps_to_string(self.as_ptr());
            let s = CStr::from_ptr(ptr).to_str().unwrap().into();
            glib::g_free(ptr as glib::gpointer);

            s
        }
    }

    pub fn get_structure(&self, idx: u32) -> Option<&Structure> {
        unsafe {
            let structure = gst::gst_caps_get_structure(self.as_ptr(), idx);
            if structure.is_null() {
                return None;
            }

            Some(Structure::from_borrowed_ptr(structure as *const gst::GstStructure))
        }
    }

    pub fn get_mut_structure(&mut self, idx: u32) -> Option<&mut Structure> {
        unsafe {
            let structure = gst::gst_caps_get_structure(self.as_ptr(), idx);
            if structure.is_null() {
                return None;
            }

            Some(Structure::from_borrowed_mut_ptr(structure as *mut gst::GstStructure))
        }
    }

    // TODO: All kinds of caps operations
}

impl fmt::Debug for Caps {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.to_string())
    }
}

impl PartialEq for Caps {
    fn eq(&self, other: &Caps) -> bool {
        (unsafe { gst::gst_caps_is_equal(self.as_ptr(), other.as_ptr()) } == glib::GTRUE)
    }
}

impl Eq for Caps {}

impl ToOwned for Caps {
    type Owned = GstRc<Caps>;

    fn to_owned(&self) -> GstRc<Caps> {
        unsafe { GstRc::from_unowned_ptr(self.as_ptr()) }
    }
}

unsafe impl Sync for Caps {}
unsafe impl Send for Caps {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;

    fn init() {
        unsafe {
            gst::gst_init(ptr::null_mut(), ptr::null_mut());
        }
    }

    #[test]
    fn test_simple() {
        init();

        let caps = Caps::new_simple("foo/bar",
                                    &[("int", 12.into()),
                                      ("bool", true.into()),
                                      ("string", "bla".into()),
                                      ("fraction", (1, 2).into()),
                                      ("array", vec![1.into(), 2.into()].into())]);
        assert_eq!(caps.to_string(),
                   "foo/bar, int=(int)12, bool=(boolean)true, string=(string)bla, \
                    fraction=(fraction)1/2, array=(int)< 1, 2 >");

        let s = caps.get_structure(0).unwrap();
        assert_eq!(s,
                   OwnedStructure::new("foo/bar",
                                       &[("int", 12.into()),
                                         ("bool", true.into()),
                                         ("string", "bla".into()),
                                         ("fraction", (1, 2).into()),
                                         ("array", vec![1.into(), 2.into()].into())])
                           .as_ref());
    }
}
