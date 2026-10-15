use anyhow::{Result, bail};
use coreaudio_sys::*;
use std::mem;
use std::os::raw::c_void;
use std::ptr;

fn address(selector: u32, scope: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMaster,
    }
}

fn get_u32(object: AudioObjectID, addr: &AudioObjectPropertyAddress) -> Option<u32> {
    let mut value: u32 = 0;
    let mut size = mem::size_of::<u32>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            addr,
            0,
            ptr::null(),
            &mut size,
            &mut value as *mut u32 as *mut c_void,
        )
    };
    (status == 0).then_some(value)
}

fn name_of(id: AudioObjectID) -> Option<String> {
    let addr = address(kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal);
    let mut cf: CFStringRef = ptr::null();
    let mut size = mem::size_of::<CFStringRef>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            id,
            &addr,
            0,
            ptr::null(),
            &mut size,
            &mut cf as *mut CFStringRef as *mut c_void,
        )
    };
    if status != 0 || cf.is_null() {
        return None;
    }
    let mut buf = vec![0u8; 512];
    let ok = unsafe {
        CFStringGetCString(
            cf,
            buf.as_mut_ptr() as *mut _,
            buf.len() as _,
            kCFStringEncodingUTF8,
        )
    };
    unsafe { CFRelease(cf as *const c_void) };
    if ok == 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..end]).into_owned())
}

fn has_output_streams(id: AudioObjectID) -> bool {
    let addr = address(kAudioDevicePropertyStreams, kAudioObjectPropertyScopeOutput);
    let mut size: u32 = 0;
    let status = unsafe { AudioObjectGetPropertyDataSize(id, &addr, 0, ptr::null(), &mut size) };
    status == 0 && size > 0
}

fn all_devices() -> Vec<AudioObjectID> {
    let addr = address(
        kAudioHardwarePropertyDevices,
        kAudioObjectPropertyScopeGlobal,
    );
    let mut size: u32 = 0;
    let status = unsafe {
        AudioObjectGetPropertyDataSize(kAudioObjectSystemObject, &addr, 0, ptr::null(), &mut size)
    };
    if status != 0 || size == 0 {
        return Vec::new();
    }
    let mut ids = vec![0 as AudioObjectID; size as usize / mem::size_of::<AudioObjectID>()];
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject,
            &addr,
            0,
            ptr::null(),
            &mut size,
            ids.as_mut_ptr() as *mut c_void,
        )
    };
    if status != 0 {
        return Vec::new();
    }
    ids
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    pub id: u32,
    pub name: String,
}

pub fn outputs() -> Vec<Output> {
    all_devices()
        .into_iter()
        .filter(|&id| has_output_streams(id))
        .filter_map(|id| name_of(id).map(|name| Output { id, name }))
        .collect()
}

/// An idle built-in speaker can report no output streams, so this matches on
/// name alone.
pub fn find(name: &str) -> Option<Output> {
    all_devices().into_iter().find_map(|id| {
        let n = name_of(id)?;
        (n == name).then_some(Output { id, name: n })
    })
}

pub fn default_output() -> Option<Output> {
    let addr = address(
        kAudioHardwarePropertyDefaultOutputDevice,
        kAudioObjectPropertyScopeGlobal,
    );
    let id = get_u32(kAudioObjectSystemObject, &addr)?;
    name_of(id).map(|name| Output { id, name })
}

pub fn set_default_output(id: u32) -> Result<()> {
    for selector in [
        kAudioHardwarePropertyDefaultOutputDevice,
        kAudioHardwarePropertyDefaultSystemOutputDevice,
    ] {
        let addr = address(selector, kAudioObjectPropertyScopeGlobal);
        let value: u32 = id;
        let status = unsafe {
            AudioObjectSetPropertyData(
                kAudioObjectSystemObject,
                &addr,
                0,
                ptr::null(),
                mem::size_of::<u32>() as u32,
                &value as *const u32 as *const c_void,
            )
        };
        if status != 0 {
            bail!("CoreAudio refused to change the default output (status {status})");
        }
    }
    Ok(())
}
