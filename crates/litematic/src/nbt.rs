//! 流式 NBT 读取器（大端，Java 版）。
//!
//! 为什么不用 fastnbt：`.litematic` 的 region compound 里 `BlockStates` 排在
//! `BlockStatePalette` **前面**，读到它时还不知道 bits/entry；而单个 BlockStates
//! 可达 6400 万个 long（实测 `流萤.litematic`）。所以需要能按需跳过大数组、
//! 不做无谓分配的读取器。

use std::collections::HashMap;
use std::io::{self, Read};

pub const TAG_END: u8 = 0;
pub const TAG_BYTE: u8 = 1;
pub const TAG_SHORT: u8 = 2;
pub const TAG_INT: u8 = 3;
pub const TAG_LONG: u8 = 4;
pub const TAG_FLOAT: u8 = 5;
pub const TAG_DOUBLE: u8 = 6;
pub const TAG_BYTE_ARRAY: u8 = 7;
pub const TAG_STRING: u8 = 8;
pub const TAG_LIST: u8 = 9;
pub const TAG_COMPOUND: u8 = 10;
pub const TAG_INT_ARRAY: u8 = 11;
pub const TAG_LONG_ARRAY: u8 = 12;

/// 已解析的 NBT 值。`LongArray` 保留为 `Vec<i64>`——BlockStates 就靠它。
#[derive(Debug, Clone)]
pub enum Value {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<i8>),
    String(String),
    List(Vec<Value>),
    Compound(Compound),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
    /// 被显式跳过的负载（调用方要求不加载）。
    Skipped,
}

pub type Compound = HashMap<String, Value>;

impl Value {
    pub fn as_i32(&self) -> Option<i32> {
        match self {
            Value::Byte(v) => Some(*v as i32),
            Value::Short(v) => Some(*v as i32),
            Value::Int(v) => Some(*v),
            Value::Long(v) => Some(*v as i32),
            _ => None,
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Byte(v) => Some(*v as i64),
            Value::Short(v) => Some(*v as i64),
            Value::Int(v) => Some(*v as i64),
            Value::Long(v) => Some(*v),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_compound(&self) -> Option<&Compound> {
        match self {
            Value::Compound(c) => Some(c),
            _ => None,
        }
    }
    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(v) => Some(v),
            _ => None,
        }
    }
    pub fn as_long_array(&self) -> Option<&[i64]> {
        match self {
            Value::LongArray(v) => Some(v),
            _ => None,
        }
    }
}

/// 读取时对某个键的处理策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// 正常读入内存
    Load,
    /// 跳过负载，只留 `Value::Skipped` 占位
    Skip,
}

/// 决定 compound 里某个键怎么读。`path` 是从根开始的键路径，最后一项是当前键。
pub type PolicyFn<'a> = &'a dyn Fn(&[String]) -> Policy;

/// 什么都加载。
pub fn load_all(_: &[String]) -> Policy {
    Policy::Load
}

/// 单个数组标签允许的最大元素数。实测最大的合法值是 `流萤.litematic` 的
/// 6440 万个 long（515 MB）；留一个数量级余量，再大就当文件损坏，
/// 避免按损坏的长度字段去分配几个 GB。
pub const DEFAULT_MAX_ARRAY_LEN: usize = 1 << 28; // 2.68 亿

pub struct Reader<R: Read> {
    inner: R,
    /// 跳过大块数据时复用的缓冲，避免反复分配
    sink: Vec<u8>,
    max_array_len: usize,
}

impl<R: Read> Reader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            sink: vec![0u8; 64 * 1024],
            max_array_len: DEFAULT_MAX_ARRAY_LEN,
        }
    }

    pub fn with_max_array_len(mut self, n: usize) -> Self {
        self.max_array_len = n;
        self
    }

    fn array_len(&self, n: i32, what: &str) -> io::Result<usize> {
        let n = n.max(0) as usize;
        if n > self.max_array_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{what} 声称有 {n} 个元素，超过上限 {}，按损坏处理", self.max_array_len),
            ));
        }
        Ok(n)
    }

    fn u8(&mut self) -> io::Result<u8> {
        let mut b = [0u8; 1];
        self.inner.read_exact(&mut b)?;
        Ok(b[0])
    }
    fn i8(&mut self) -> io::Result<i8> {
        Ok(self.u8()? as i8)
    }
    fn u16(&mut self) -> io::Result<u16> {
        let mut b = [0u8; 2];
        self.inner.read_exact(&mut b)?;
        Ok(u16::from_be_bytes(b))
    }
    fn i16(&mut self) -> io::Result<i16> {
        Ok(self.u16()? as i16)
    }
    fn i32(&mut self) -> io::Result<i32> {
        let mut b = [0u8; 4];
        self.inner.read_exact(&mut b)?;
        Ok(i32::from_be_bytes(b))
    }
    fn i64(&mut self) -> io::Result<i64> {
        let mut b = [0u8; 8];
        self.inner.read_exact(&mut b)?;
        Ok(i64::from_be_bytes(b))
    }
    fn f32(&mut self) -> io::Result<f32> {
        Ok(f32::from_bits(self.i32()? as u32))
    }
    fn f64(&mut self) -> io::Result<f64> {
        Ok(f64::from_bits(self.i64()? as u64))
    }

    fn string(&mut self) -> io::Result<String> {
        let n = self.u16()? as usize;
        let mut buf = vec![0u8; n];
        self.inner.read_exact(&mut buf)?;
        // MC 用的是 modified UTF-8；ASCII 与常规 UTF-8 部分完全一致，
        // 中文蓝图名走的就是常规 UTF-8 路径。非法字节退化为替换字符而非报错。
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }

    fn skip_bytes(&mut self, mut n: u64) -> io::Result<()> {
        while n > 0 {
            let chunk = n.min(self.sink.len() as u64) as usize;
            self.inner.read_exact(&mut self.sink[..chunk])?;
            n -= chunk as u64;
        }
        Ok(())
    }

    /// 读根标签。返回 (根名, 根值)。
    pub fn read_root(&mut self, policy: PolicyFn) -> io::Result<(String, Value)> {
        let tag = self.u8()?;
        if tag != TAG_COMPOUND {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("根标签应为 Compound，实际为 {tag}"),
            ));
        }
        let name = self.string()?;
        let mut path: Vec<String> = Vec::new();
        let v = self.payload(TAG_COMPOUND, &mut path, policy)?;
        Ok((name, v))
    }

    fn payload(
        &mut self,
        tag: u8,
        path: &mut Vec<String>,
        policy: PolicyFn,
    ) -> io::Result<Value> {
        Ok(match tag {
            TAG_BYTE => Value::Byte(self.i8()?),
            TAG_SHORT => Value::Short(self.i16()?),
            TAG_INT => Value::Int(self.i32()?),
            TAG_LONG => Value::Long(self.i64()?),
            TAG_FLOAT => Value::Float(self.f32()?),
            TAG_DOUBLE => Value::Double(self.f64()?),
            TAG_BYTE_ARRAY => {
                let raw = self.i32()?;
                let n = self.array_len(raw, "ByteArray")?;
                let mut v = vec![0u8; n];
                self.inner.read_exact(&mut v)?;
                Value::ByteArray(v.into_iter().map(|b| b as i8).collect())
            }
            TAG_STRING => Value::String(self.string()?),
            TAG_LIST => {
                let elem = self.u8()?;
                let raw = self.i32()?;
                let n = self.array_len(raw, "List")?;
                // 空 List 的元素类型常被写成 TAG_END，此时不能再去读负载
                if elem == TAG_END {
                    Value::List(Vec::new())
                } else {
                    let mut items = Vec::with_capacity(n.min(4096));
                    for _ in 0..n {
                        items.push(self.payload(elem, path, policy)?);
                    }
                    Value::List(items)
                }
            }
            TAG_COMPOUND => {
                let mut map = Compound::new();
                loop {
                    let t = self.u8()?;
                    if t == TAG_END {
                        break;
                    }
                    let key = self.string()?;
                    path.push(key);
                    let value = match policy(path) {
                        Policy::Skip => {
                            self.skip_payload(t)?;
                            Value::Skipped
                        }
                        Policy::Load => self.payload(t, path, policy)?,
                    };
                    let key = path.pop().expect("刚压入的键必然存在");
                    map.insert(key, value);
                }
                Value::Compound(map)
            }
            TAG_INT_ARRAY => {
                let raw = self.i32()?;
                let n = self.array_len(raw, "IntArray")?;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.i32()?);
                }
                Value::IntArray(v)
            }
            TAG_LONG_ARRAY => {
                let raw = self.i32()?;
                let n = self.array_len(raw, "LongArray")?;
                let mut v: Vec<i64> = Vec::with_capacity(n);
                // 批量读，避免逐个 read_exact 的系统调用开销
                let mut buf = vec![0u8; 8 * 8192];
                let mut left = n;
                while left > 0 {
                    let take = left.min(8192);
                    let bytes = take * 8;
                    self.inner.read_exact(&mut buf[..bytes])?;
                    for c in buf[..bytes].chunks_exact(8) {
                        v.push(i64::from_be_bytes(c.try_into().unwrap()));
                    }
                    left -= take;
                }
                Value::LongArray(v)
            }
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("未知 NBT 标签 {other}"),
                ))
            }
        })
    }

    /// 跳过一个负载而不构造 Value。
    fn skip_payload(&mut self, tag: u8) -> io::Result<()> {
        match tag {
            TAG_BYTE => self.skip_bytes(1)?,
            TAG_SHORT => self.skip_bytes(2)?,
            TAG_INT | TAG_FLOAT => self.skip_bytes(4)?,
            TAG_LONG | TAG_DOUBLE => self.skip_bytes(8)?,
            TAG_BYTE_ARRAY => {
                let n = self.i32()?.max(0) as u64;
                self.skip_bytes(n)?;
            }
            TAG_STRING => {
                let n = self.u16()? as u64;
                self.skip_bytes(n)?;
            }
            TAG_LIST => {
                let elem = self.u8()?;
                let n = self.i32()?.max(0);
                if elem != TAG_END {
                    for _ in 0..n {
                        self.skip_payload(elem)?;
                    }
                }
            }
            TAG_COMPOUND => loop {
                let t = self.u8()?;
                if t == TAG_END {
                    break;
                }
                let n = self.u16()? as u64;
                self.skip_bytes(n)?;
                self.skip_payload(t)?;
            },
            TAG_INT_ARRAY => {
                let n = self.i32()?.max(0) as u64;
                self.skip_bytes(n * 4)?;
            }
            TAG_LONG_ARRAY => {
                let n = self.i32()?.max(0) as u64;
                self.skip_bytes(n * 8)?;
            }
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("未知 NBT 标签 {other}"),
                ))
            }
        }
        Ok(())
    }
}
