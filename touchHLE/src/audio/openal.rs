/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Wrapper for OpenAL usage.
use al_sys::alc_types::{ALCcontext, ALCdevice};
use std::cell::Cell;
use std::marker::PhantomData;
use std::rc::{Rc, Weak};
use touchHLE_openal_soft_wrapper as al_sys;
use touchHLE_openal_soft_wrapper::alc_types::ALCint;

pub use al_sys::al_defines::*;
pub use al_sys::al_types;
pub use al_sys::alc_defines::*;
pub use al_sys::alc_types;
pub use al_sys::{alcCloseDevice, alcGetError, alcGetString, alcOpenDevice};

use al_types::*;

static OPENALMANAGER_INSTANCE_EXISTS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
#[derive(Debug)]
struct ContextVolume {
    context: *mut ALCcontext,
    guest_gain: Cell<ALfloat>,
    muted: Rc<Cell<bool>>,
}
impl ContextVolume {
    fn output_gain(&self) -> ALfloat {
        if self.muted.get() {
            0.0
        } else {
            self.guest_gain.get()
        }
    }
}
pub struct OpenALManager {
    muted: Rc<Cell<bool>>,
    contexts: Vec<Weak<ContextVolume>>,
}
impl OpenALManager {
    pub fn new() -> Result<Self, String> {
        if OPENALMANAGER_INSTANCE_EXISTS.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return Err("Only one OpenALManager can exist at a time!".to_string());
        }
        Ok(Self {
            muted: Rc::new(Cell::new(false)),
            contexts: Vec::new(),
        })
    }
    pub fn is_muted(&self) -> bool {
        self.muted.get()
    }
    pub fn set_muted(&mut self, muted: bool) {
        self.muted.set(muted);
        unsafe {
            let previous = al_sys::alcGetCurrentContext();
            self.contexts.retain(|context| {
                if let Some(context) = context.upgrade() {
                    if al_sys::alcMakeContextCurrent(context.context) == ALC_TRUE {
                        al_sys::alListenerf(AL_GAIN, context.output_gain());
                    }
                    true
                } else {
                    false
                }
            });
            al_sys::alcMakeContextCurrent(previous);
        }
    }
}

impl Drop for OpenALManager {
    fn drop(&mut self) {
        OPENALMANAGER_INSTANCE_EXISTS.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

#[derive(Debug)]
pub struct OpenALContext {
    context: *mut ALCcontext,
    // We don't need to guarantee the lifetime or thread-safety of the device -
    // OpenAL (should) do that for us.
    device: *mut ALCdevice,
    volume: Rc<ContextVolume>,
}

impl OpenALContext {
    pub fn new(_manager: &mut OpenALManager) -> Result<Self, String> {
        let device = unsafe { al_sys::alcOpenDevice(std::ptr::null()) };
        if device.is_null() {
            return Err("Could not open OpenAL device".to_string());
        }
        unsafe { Self::new_with_device_and_attrlist(_manager, device, std::ptr::null()) }
    }

    pub unsafe fn new_with_device_and_attrlist(
        _manager: &mut OpenALManager,
        device: *mut ALCdevice,
        attrlist: *const ALCint,
    ) -> Result<Self, String> {
        let context = unsafe { al_sys::alcCreateContext(device, attrlist) };
        if context.is_null() {
            return Err("Could not open OpenAL context".to_string());
        }
        log_dbg!(
            "New OpenAL device ({:?}) and context ({:?})",
            device,
            context
        );
        let volume = Rc::new(ContextVolume {
            context,
            guest_gain: Cell::new(1.0),
            muted: _manager.muted.clone(),
        });
        _manager.contexts.push(Rc::downgrade(&volume));
        let previous = al_sys::alcGetCurrentContext();
        al_sys::alcMakeContextCurrent(context);
        al_sys::alListenerf(AL_GAIN, volume.output_gain());
        al_sys::alcMakeContextCurrent(previous);
        Ok(Self {
            context,
            device,
            volume,
        })
    }

    pub fn make_current<'al_ctx, 'manager: 'al_ctx>(
        &'al_ctx mut self,
        _manager: &'manager mut OpenALManager,
    ) -> OpenAL<'al_ctx> {
        // OpenALsoft already caches the current context, no need to
        // check ourselves.
        let ret = unsafe { al_sys::alcMakeContextCurrent(self.context) };
        assert!(ret == ALC_TRUE);
        OpenAL {
            _al_lifetime: PhantomData,
            volume: &self.volume,
        }
    }

    pub fn SuspendContext(&mut self) {
        unsafe { al_sys::alcSuspendContext(self.context) };
    }

    pub fn ProcessContext(&mut self) {
        unsafe { al_sys::alcProcessContext(self.context) };
    }

    pub fn GetContextsDevice(&self) -> *mut ALCdevice {
        self.device
    }

    // According to both OpenAL docs and OpenALsoft source code, this is
    // context independent, and can be called without one active.
    pub unsafe fn GetEnumValue(enumName: *const ALchar) -> ALenum {
        al_sys::alGetEnumValue(enumName)
    }
}

impl Drop for OpenALContext {
    fn drop(&mut self) {
        unsafe { al_sys::alcDestroyContext(self.context) };
    }
}

pub struct OpenAL<'al_ctx> {
    _al_lifetime: PhantomData<&'al_ctx ()>,
    volume: &'al_ctx ContextVolume,
}

impl OpenAL<'_> {
    pub unsafe fn GetError(&self) -> ALenum {
        al_sys::alGetError()
    }

    pub unsafe fn DistanceModel(&self, value: ALenum) {
        al_sys::alDistanceModel(value)
    }

    pub unsafe fn IsBuffer(&self, buffer: ALuint) -> ALboolean {
        al_sys::alIsBuffer(buffer)
    }
    pub unsafe fn IsSource(&self, source: ALuint) -> ALboolean {
        al_sys::alIsSource(source)
    }
    pub unsafe fn IsExtensionPresent(&self, extName: *const ALchar) -> ALboolean {
        al_sys::alIsExtensionPresent(extName)
    }

    pub unsafe fn Enable(&self, capability: ALenum) {
        al_sys::alEnable(capability)
    }

    pub unsafe fn GetBufferi(&self, buffer: ALuint, param: ALenum, value: *const ALint) {
        al_sys::alGetBufferi(buffer, param, value)
    }

    pub unsafe fn Listenerf(&self, param: ALenum, value: ALfloat) {
        if param == AL_GAIN && value.is_finite() && value >= 0.0 {
            self.volume.guest_gain.set(value);
            al_sys::alListenerf(param, self.volume.output_gain());
        } else {
            al_sys::alListenerf(param, value)
        }
    }
    pub unsafe fn Listener3f(
        &self,
        param: ALenum,
        value1: ALfloat,
        value2: ALfloat,
        value3: ALfloat,
    ) {
        al_sys::alListener3f(param, value1, value2, value3)
    }
    pub unsafe fn Listenerfv(&self, param: ALenum, values: *const ALfloat) {
        if param == AL_GAIN && !values.is_null() {
            self.Listenerf(param, *values);
        } else {
            al_sys::alListenerfv(param, values)
        }
    }
    pub unsafe fn Listeneri(&self, param: ALenum, value: ALint) {
        if param == AL_GAIN {
            self.Listenerf(param, value as ALfloat);
        } else {
            al_sys::alListeneri(param, value)
        }
    }
    pub unsafe fn Listener3i(&self, param: ALenum, value1: ALint, value2: ALint, value3: ALint) {
        al_sys::alListener3i(param, value1, value2, value3)
    }
    pub unsafe fn Listeneriv(&self, param: ALenum, values: *const ALint) {
        if param == AL_GAIN && !values.is_null() {
            self.Listeneri(param, *values);
        } else {
            al_sys::alListeneriv(param, values)
        }
    }

    pub unsafe fn GetListenerf(&self, param: ALenum, value: *mut ALfloat) {
        if param == AL_GAIN && !value.is_null() {
            *value = self.volume.guest_gain.get();
        } else {
            al_sys::alGetListenerf(param, value)
        }
    }
    pub unsafe fn GetListener3f(
        &self,
        param: ALenum,
        value1: *mut ALfloat,
        value2: *mut ALfloat,
        value3: *mut ALfloat,
    ) {
        al_sys::alGetListener3f(param, value1, value2, value3)
    }
    pub unsafe fn GetListenerfv(&self, param: ALenum, values: *mut ALfloat) {
        if param == AL_GAIN && !values.is_null() {
            *values = self.volume.guest_gain.get();
        } else {
            al_sys::alGetListenerfv(param, values)
        }
    }
    pub unsafe fn GetListeneri(&self, param: ALenum, value: *mut ALint) {
        if param == AL_GAIN && !value.is_null() {
            *value = self.volume.guest_gain.get() as ALint;
        } else {
            al_sys::alGetListeneri(param, value)
        }
    }
    pub unsafe fn GetListener3i(
        &self,
        param: ALenum,
        value1: *mut ALint,
        value2: *mut ALint,
        value3: *mut ALint,
    ) {
        al_sys::alGetListener3i(param, value1, value2, value3)
    }
    pub unsafe fn GetListeneriv(&self, param: ALenum, values: *mut ALint) {
        if param == AL_GAIN && !values.is_null() {
            *values = self.volume.guest_gain.get() as ALint;
        } else {
            al_sys::alGetListeneriv(param, values)
        }
    }

    pub unsafe fn GenSources(&self, n: ALsizei, sources: *mut ALuint) {
        al_sys::alGenSources(n, sources)
    }
    pub unsafe fn DeleteSources(&self, n: ALsizei, sources: *const ALuint) {
        al_sys::alDeleteSources(n, sources)
    }

    pub unsafe fn Sourcef(&self, source: ALuint, param: ALenum, value: ALfloat) {
        al_sys::alSourcef(source, param, value)
    }
    pub unsafe fn Source3f(
        &self,
        source: ALuint,
        param: ALenum,
        value1: ALfloat,
        value2: ALfloat,
        value3: ALfloat,
    ) {
        al_sys::alSource3f(source, param, value1, value2, value3)
    }
    pub unsafe fn Sourcefv(&self, source: ALuint, param: ALenum, values: *const ALfloat) {
        al_sys::alSourcefv(source, param, values)
    }
    pub unsafe fn Sourcei(&self, source: ALuint, param: ALenum, value: ALint) {
        al_sys::alSourcei(source, param, value)
    }
    pub unsafe fn Source3i(
        &self,
        source: ALuint,
        param: ALenum,
        value1: ALint,
        value2: ALint,
        value3: ALint,
    ) {
        al_sys::alSource3i(source, param, value1, value2, value3)
    }
    pub unsafe fn Sourceiv(&self, source: ALuint, param: ALenum, values: *const ALint) {
        al_sys::alSourceiv(source, param, values)
    }

    pub unsafe fn GetSourcef(&self, source: ALuint, param: ALenum, value: *mut ALfloat) {
        al_sys::alGetSourcef(source, param, value)
    }
    pub unsafe fn GetSource3f(
        &self,
        source: ALuint,
        param: ALenum,
        value1: *mut ALfloat,
        value2: *mut ALfloat,
        value3: *mut ALfloat,
    ) {
        al_sys::alGetSource3f(source, param, value1, value2, value3)
    }
    pub unsafe fn GetSourcefv(&self, source: ALuint, param: ALenum, values: *mut ALfloat) {
        al_sys::alGetSourcefv(source, param, values)
    }
    pub unsafe fn GetSourcei(&self, source: ALuint, param: ALenum, value: *mut ALint) {
        al_sys::alGetSourcei(source, param, value)
    }
    pub unsafe fn GetSource3i(
        &self,
        source: ALuint,
        param: ALenum,
        value1: *mut ALint,
        value2: *mut ALint,
        value3: *mut ALint,
    ) {
        al_sys::alGetSource3i(source, param, value1, value2, value3)
    }
    pub unsafe fn GetSourceiv(&self, source: ALuint, param: ALenum, values: *mut ALint) {
        al_sys::alGetSourceiv(source, param, values)
    }

    pub unsafe fn SourcePlay(&self, source: ALuint) {
        al_sys::alSourcePlay(source)
    }
    pub unsafe fn SourcePause(&self, source: ALuint) {
        al_sys::alSourcePause(source)
    }
    pub unsafe fn SourceStop(&self, source: ALuint) {
        al_sys::alSourceStop(source)
    }
    pub unsafe fn SourceRewind(&self, source: ALuint) {
        al_sys::alSourceRewind(source)
    }

    pub unsafe fn SourcePlayv(&self, n: ALsizei, sources: *const ALuint) {
        al_sys::alSourcePlayv(n, sources)
    }
    pub unsafe fn SourcePausev(&self, n: ALsizei, sources: *const ALuint) {
        al_sys::alSourcePausev(n, sources)
    }
    pub unsafe fn SourceStopv(&self, n: ALsizei, sources: *const ALuint) {
        al_sys::alSourceStopv(n, sources)
    }
    pub unsafe fn SourceRewindv(&self, n: ALsizei, sources: *const ALuint) {
        al_sys::alSourceRewindv(n, sources)
    }

    pub unsafe fn SourceQueueBuffers(&self, source: ALuint, nb: ALsizei, buffers: *const ALuint) {
        al_sys::alSourceQueueBuffers(source, nb, buffers)
    }
    pub unsafe fn SourceUnqueueBuffers(&self, source: ALuint, nb: ALsizei, buffers: *mut ALuint) {
        al_sys::alSourceUnqueueBuffers(source, nb, buffers)
    }

    pub unsafe fn GenBuffers(&self, n: ALsizei, buffers: *mut ALuint) {
        al_sys::alGenBuffers(n, buffers)
    }
    pub unsafe fn DeleteBuffers(&self, n: ALsizei, buffers: *const ALuint) {
        al_sys::alDeleteBuffers(n, buffers)
    }

    pub unsafe fn BufferData(
        &self,
        buffer: ALuint,
        format: ALenum,
        data: *const ALvoid,
        size: ALsizei,
        samplerate: ALsizei,
    ) {
        al_sys::alBufferData(buffer, format, data, size, samplerate)
    }

    pub unsafe fn DopplerFactor(&self, dopplerFactor: ALfloat) {
        al_sys::alDopplerFactor(dopplerFactor)
    }
    pub unsafe fn DopplerVelocity(&self, dopplerVelocity: ALfloat) {
        al_sys::alDopplerVelocity(dopplerVelocity)
    }
    pub unsafe fn SpeedOfSound(&self, speed: ALfloat) {
        al_sys::alSpeedOfSound(speed)
    }
}

#[cfg(test)]
mod host_volume_tests {
    use super::*;
    #[test]
    fn host_mute_updates_existing_and_new_contexts_without_changing_guest_gain() {
        let mut manager = OpenALManager::new().unwrap();
        let mut first = OpenALContext::new(&mut manager).unwrap();
        {
            let al = first.make_current(&mut manager);
            unsafe {
                al.Listenerf(AL_GAIN, 0.4);
            }
        }
        manager.set_muted(true);
        let mut second = OpenALContext::new(&mut manager).unwrap();
        for (context, expected) in [(&mut first, 0.4), (&mut second, 1.0)] {
            let al = context.make_current(&mut manager);
            let mut physical = -1.0;
            let mut guest = -1.0;
            unsafe {
                al_sys::alGetListenerf(AL_GAIN, &mut physical);
                al.GetListenerf(AL_GAIN, &mut guest);
            }
            assert_eq!(physical, 0.0);
            assert_eq!(guest, expected);
        }
        {
            let al = first.make_current(&mut manager);
            unsafe {
                al.Listenerf(AL_GAIN, 0.7);
            }
        }
        manager.set_muted(false);
        let _al = first.make_current(&mut manager);
        let mut physical = -1.0;
        unsafe {
            al_sys::alGetListenerf(AL_GAIN, &mut physical);
        }
        assert_eq!(physical, 0.7);
    }

    #[test]
    fn mute_preserves_guest_gain_and_restores_latest_value() {
        let muted = Rc::new(Cell::new(false));
        let state = ContextVolume {
            context: std::ptr::null_mut(),
            guest_gain: Cell::new(0.4),
            muted: muted.clone(),
        };
        assert_eq!(state.output_gain(), 0.4);
        muted.set(true);
        assert_eq!(state.output_gain(), 0.0);
        state.guest_gain.set(0.7);
        assert_eq!(state.output_gain(), 0.0);
        muted.set(false);
        assert_eq!(state.output_gain(), 0.7);
    }
}
