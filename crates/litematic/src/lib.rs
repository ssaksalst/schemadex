//! 读取 Litematica `.litematic` 蓝图。
//!
//! 设计约束全部来自对 2138 个真实蓝图的实测，细节见各模块头部注释。
//! 三条最重要的：
//! - BlockStates 用**跨 long 边界**打包（不是 MC 1.16+ 区块那套）
//! - `Metadata` 里的统计数字**会被篡改**，一律自己算
//! - 单 region 体积可达 5.15 亿，索引阶段必须能跳过 BlockStates

pub mod bitarray;
pub mod materials;
pub mod nbt;
pub mod schematic;

pub use materials::{MaterialList, MaterialOptions};
pub use schematic::{BlockState, LoadMode, Region, Schematic, Vec3i};
