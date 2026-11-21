use std::{io, ptr};
use windows_sys::Win32::Networking::WinInet::{
    InternetSetOptionW, INTERNET_OPTION_REFRESH, INTERNET_OPTION_SETTINGS_CHANGED,
};
use winreg::{
    enums::{HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE},
    types::ToRegValue,
    RegKey, RegValue,
};

const KEYS: [&str; 3] = ["ProxyEnable", "ProxyServer", "ProxyOverride"];
const BYPASS:&str="localhost;127.*;10.*;172.16.*;172.17.*;172.18.*;172.19.*;172.20.*;172.21.*;172.22.*;172.23.*;172.24.*;172.25.*;172.26.*;172.27.*;172.28.*;172.29.*;172.30.*;172.31.*;192.168.*;<local>";
type Snapshot = [Option<RegValue>; 3];
trait Settings {
    fn read(&self, key: &str) -> io::Result<Option<RegValue>>;
    fn write(&mut self, key: &str, value: Option<&RegValue>) -> io::Result<()>;
    fn refresh(&self);
}
impl Settings for RegKey {
    fn read(&self, key: &str) -> io::Result<Option<RegValue>> {
        match self.get_raw_value(key) {
            Ok(v) => Ok(Some(v)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
    fn write(&mut self, key: &str, value: Option<&RegValue>) -> io::Result<()> {
        match value {
            Some(v) => self.set_raw_value(key, v),
            None => match self.delete_value(key) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                r => r,
            },
        }
    }
    fn refresh(&self) {
        unsafe {
            InternetSetOptionW(
                ptr::null_mut(),
                INTERNET_OPTION_SETTINGS_CHANGED,
                ptr::null_mut(),
                0,
            );
            InternetSetOptionW(ptr::null_mut(), INTERNET_OPTION_REFRESH, ptr::null_mut(), 0);
        }
    }
}
fn internet_settings() -> io::Result<RegKey> {
    RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(
        r"Software\Microsoft\Windows\CurrentVersion\Internet Settings",
        KEY_QUERY_VALUE | KEY_SET_VALUE,
    )
}

#[derive(Default)]
pub struct SystemProxy {
    saved: Option<Snapshot>,
}
impl SystemProxy {
    pub fn enable(&mut self, port: u16) -> io::Result<()> {
        self.install(&mut internet_settings()?, port)
    }
    pub fn disable(&mut self) -> io::Result<()> {
        if self.saved.is_none() {
            return Ok(());
        }
        self.restore(&mut internet_settings()?)
    }
    fn install(&mut self, key: &mut impl Settings, port: u16) -> io::Result<()> {
        if self.saved.is_none() {
            self.saved = Some([key.read(KEYS[0])?, key.read(KEYS[1])?, key.read(KEYS[2])?]);
        }
        let values = [
            1u32.to_reg_value(),
            format!("127.0.0.1:{port}").to_reg_value(),
            BYPASS.to_reg_value(),
        ];
        for i in [1, 2, 0] {
            if let Err(e) = key.write(KEYS[i], Some(&values[i])) {
                self.restore(key)?;
                return Err(e);
            }
        }
        key.refresh();
        Ok(())
    }
    fn restore(&mut self, key: &mut impl Settings) -> io::Result<()> {
        let Some(saved) = &self.saved else {
            return Ok(());
        };
        let mut first_error = None;
        for i in [1, 2, 0] {
            if let Err(e) = key.write(KEYS[i], saved[i].as_ref()) {
                first_error.get_or_insert(e);
            }
        }
        key.refresh();
        if let Some(e) = first_error {
            return Err(e);
        }
        self.saved = None;
        Ok(())
    }
}
impl Drop for SystemProxy {
    fn drop(&mut self) {
        let _ = self.disable();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    #[derive(Default)]
    struct Memory {
        values: BTreeMap<String, RegValue>,
        writes: usize,
        fail_at: Option<usize>,
    }
    impl Settings for Memory {
        fn read(&self, key: &str) -> io::Result<Option<RegValue>> {
            Ok(self.values.get(key).map(|v| RegValue {
                bytes: v.bytes.clone(),
                vtype: v.vtype.clone(),
            }))
        }
        fn write(&mut self, key: &str, v: Option<&RegValue>) -> io::Result<()> {
            self.writes += 1;
            if self.fail_at == Some(self.writes) {
                return Err(io::Error::other("injected failure"));
            }
            if let Some(v) = v {
                self.values.insert(
                    key.into(),
                    RegValue {
                        bytes: v.bytes.clone(),
                        vtype: v.vtype.clone(),
                    },
                );
            } else {
                self.values.remove(key);
            }
            Ok(())
        }
        fn refresh(&self) {}
    }
    #[test]
    fn rollback_and_repeated_cleanup_preserve_existing_agent() {
        for fail in [None, Some(1), Some(2), Some(3)] {
            let mut sys = Memory::default();
            sys.values.insert(KEYS[0].into(), 1u32.to_reg_value());
            sys.values.insert(KEYS[1].into(), "previous".to_reg_value());
            sys.fail_at = fail;
            let mut manager = SystemProxy::default();
            manager.restore(&mut sys).unwrap();
            assert_eq!(sys.writes, 0);
            assert_eq!(manager.install(&mut sys, 17890).is_err(), fail.is_some());
            manager.restore(&mut sys).unwrap();
            assert_eq!(sys.values.get(KEYS[0]), Some(&1u32.to_reg_value()));
            assert_eq!(sys.values.get(KEYS[1]), Some(&"previous".to_reg_value()));
            assert!(!sys.values.contains_key(KEYS[2]));
            let writes = sys.writes;
            manager.restore(&mut sys).unwrap();
            assert_eq!(sys.writes, writes);
        }
    }
}
