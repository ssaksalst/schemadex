//! `.litematic` 结构解析。
//!
//! 实测结论（2138 个真实蓝图）驱动的几个设计取舍：
//! - `Metadata` 里的 `TotalVolume` / `TotalBlocks` / `MinecraftDataVersion`
//!   **会被篡改**（见到 6 个文件写着 `1919810`，2 个写着 dv=`20060210`），
//!   一律只当声明值，真实数字全部自己算。
//! - `Size` 有负值的 region 占 1757/2708，是常态而非边缘情况。
//! - 单文件 region 数最多见到 98 个（世吞类蓝图）。
//! - `TileEntities` 里 77 万个条目没有 `id` 字段，方块类型得从调色板反推。

use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use flate2::read::GzDecoder;

use crate::bitarray::{bits_per_entry, required_longs, BitArray};
use crate::nbt::{Compound, Policy, Reader, Value};

/// 一个方块状态：命名空间 ID + 属性。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockState {
    pub name: String,
    /// 用 BTreeMap 保证顺序稳定，便于做内容哈希与去重
    pub properties: BTreeMap<String, String>,
}

impl BlockState {
    pub fn is_air(&self) -> bool {
        matches!(
            self.name.as_str(),
            "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
        )
    }
    pub fn prop(&self, key: &str) -> Option<&str> {
        self.properties.get(key).map(|s| s.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Vec3i {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl Vec3i {
    fn from_nbt(c: &Compound) -> Option<Self> {
        Some(Self {
            x: c.get("x")?.as_i32()?,
            y: c.get("y")?.as_i32()?,
            z: c.get("z")?.as_i32()?,
        })
    }
    pub fn abs(self) -> Self {
        Self { x: self.x.abs(), y: self.y.abs(), z: self.z.abs() }
    }
    pub fn volume(self) -> u64 {
        (self.x.abs() as u64) * (self.y.abs() as u64) * (self.z.abs() as u64)
    }
}

/// Metadata 中的声明值。**不可信**，仅用于展示与对照。
#[derive(Debug, Clone, Default)]
pub struct Metadata {
    pub name: Option<String>,
    pub author: Option<String>,
    pub description: Option<String>,
    pub time_created: Option<i64>,
    pub time_modified: Option<i64>,
    pub declared_total_blocks: Option<i64>,
    pub declared_total_volume: Option<i64>,
    pub declared_region_count: Option<i32>,
    pub enclosing_size: Option<Vec3i>,
    /// 只有 24/2138 个文件带内嵌预览图，指望不上，缩略图得自己渲染
    pub preview_image_argb: Option<Vec<i32>>,
}

/// 容器里的一摞物品。
#[derive(Debug, Clone)]
pub struct ItemStack {
    pub id: String,
    pub count: i64,
}

#[derive(Debug, Clone)]
pub struct TileEntity {
    pub pos: Vec3i,
    /// 旧版 Litematica 不写这个字段（实测 77 万条缺失），需要时从调色板反推
    pub id: Option<String>,
    pub items: Vec<ItemStack>,
}

pub struct Region {
    pub name: String,
    pub position: Vec3i,
    /// 原始 Size，可能为负
    pub size: Vec3i,
    pub palette: Vec<BlockState>,
    /// 未加载时为空（索引模式）
    pub block_states: Vec<i64>,
    pub tile_entities: Vec<TileEntity>,
    pub entity_count: usize,
}

impl Region {
    /// 各轴长度的绝对值——数组寻址用这个。
    pub fn extent(&self) -> Vec3i {
        self.size.abs()
    }

    pub fn volume(&self) -> u64 {
        self.size.volume()
    }

    /// region 在蓝图局部坐标系中的最小角。
    /// Size 为负时表示从 position 往负方向延伸。
    pub fn min_corner(&self) -> Vec3i {
        let f = |p: i32, s: i32| if s >= 0 { p } else { p + s + 1 };
        Vec3i {
            x: f(self.position.x, self.size.x),
            y: f(self.position.y, self.size.y),
            z: f(self.position.z, self.size.z),
        }
    }

    pub fn bits(&self) -> u32 {
        bits_per_entry(self.palette.len())
    }

    /// BlockStates 的长度是否与 Size/调色板 自洽。
    /// 实测 2708 个 region 全部自洽，不自洽说明文件有问题。
    pub fn is_consistent(&self) -> bool {
        self.block_states.is_empty()
            || self.block_states.len() == required_longs(self.volume(), self.bits())
    }

    /// 调色板索引直方图。内存 O(调色板)，可处理 5 亿体积的蓝图。
    pub fn palette_histogram(&self) -> Vec<u64> {
        if self.block_states.is_empty() {
            return vec![0; self.palette.len()];
        }
        BitArray::new(&self.block_states, self.bits())
            .histogram(self.volume(), self.palette.len())
    }

    /// 线性索引 → 局部坐标。Litematica 的顺序是 y 外层、z 中层、x 内层。
    #[inline]
    pub fn index_to_pos(&self, index: u64) -> Vec3i {
        let e = self.extent();
        let sx = e.x.max(1) as u64;
        let sz = e.z.max(1) as u64;
        let y = index / (sx * sz);
        let rem = index % (sx * sz);
        Vec3i { x: (rem % sx) as i32, y: y as i32, z: (rem / sx) as i32 }
    }

    #[inline]
    pub fn pos_to_index(&self, x: i32, y: i32, z: i32) -> u64 {
        let e = self.extent();
        (y as u64) * (e.x as u64) * (e.z as u64) + (z as u64) * (e.x as u64) + (x as u64)
    }

    /// 按 (index, 调色板索引) 顺序遍历所有方块。
    pub fn for_each_block(&self, f: impl FnMut(u64, u32)) {
        if self.block_states.is_empty() {
            return;
        }
        BitArray::new(&self.block_states, self.bits()).for_each(self.volume(), f);
    }

    /// 只遍历索引区间 `[start, start+count)`。
    ///
    /// 索引顺序是 y 最外层，所以「只要第 y 层」= 一段连续区间，
    /// 巨型蓝图取单层不必扫全部方块。
    pub fn for_each_block_range(&self, start: u64, count: u64, f: impl FnMut(u64, u32)) {
        if self.block_states.is_empty() {
            return;
        }
        let end = (start + count).min(self.volume());
        if end <= start {
            return;
        }
        BitArray::new(&self.block_states, self.bits()).for_each_range(start, end - start, f);
    }
}

pub struct Schematic {
    pub path: PathBuf,
    pub version: i32,
    pub sub_version: Option<i32>,
    pub data_version: i32,
    pub metadata: Metadata,
    pub regions: Vec<Region>,
    /// 索引模式下 BlockStates 未加载
    pub blocks_loaded: bool,
}

/// 加载模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadMode {
    /// 只读结构信息：尺寸、调色板、region 数。跳过 BlockStates 与 TileEntities。
    /// 用于快速建索引——不必为了知道蓝图多大而解压 515 MB 的方块数据。
    Index,
    /// 全量：BlockStates + TileEntities。材料清单与缩略图需要。
    Full,
}

impl Schematic {
    pub fn load(path: impl AsRef<Path>, mode: LoadMode) -> Result<Self> {
        let path = path.as_ref();
        let file = File::open(path).with_context(|| format!("打不开 {}", path.display()))?;
        let gz = GzDecoder::new(BufReader::with_capacity(1 << 20, file));
        let mut reader = Reader::new(BufReader::with_capacity(1 << 20, gz));

        let policy = |p: &[String]| -> Policy {
            if mode == LoadMode::Full {
                return Policy::Load;
            }
            // 索引模式：Regions.<name>.{BlockStates,TileEntities,Entities,Pending*} 全跳
            if p.len() == 3 && p[0] == "Regions" {
                return match p[2].as_str() {
                    "BlockStates" | "TileEntities" | "Entities" | "PendingBlockTicks"
                    | "PendingFluidTicks" => Policy::Skip,
                    _ => Policy::Load,
                };
            }
            // 预览图在索引阶段也不需要
            if p.len() == 2 && p[0] == "Metadata" && p[1] == "PreviewImageData" {
                return Policy::Skip;
            }
            Policy::Load
        };

        let (_, root) = reader
            .read_root(&policy)
            .with_context(|| format!("解析 NBT 失败: {}", path.display()))?;
        let root = root
            .as_compound()
            .ok_or_else(|| anyhow!("根不是 Compound: {}", path.display()))?;

        let version = root.get("Version").and_then(Value::as_i32).unwrap_or(0);
        let sub_version = root.get("SubVersion").and_then(Value::as_i32);
        let data_version = root
            .get("MinecraftDataVersion")
            .and_then(Value::as_i32)
            .unwrap_or(0);

        let metadata = root
            .get("Metadata")
            .and_then(Value::as_compound)
            .map(parse_metadata)
            .unwrap_or_default();

        let regions_nbt = root
            .get("Regions")
            .and_then(Value::as_compound)
            .ok_or_else(|| anyhow!("缺少 Regions: {}", path.display()))?;

        let mut regions = Vec::with_capacity(regions_nbt.len());
        for (name, val) in regions_nbt {
            let c = match val.as_compound() {
                Some(c) => c,
                None => continue,
            };
            regions.push(parse_region(name.clone(), c)?);
        }
        // region 在 NBT 里是无序 map，排序保证输出稳定可复现
        regions.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(Self {
            path: path.to_path_buf(),
            version,
            sub_version,
            data_version,
            metadata,
            regions,
            blocks_loaded: mode == LoadMode::Full,
        })
    }

    /// 实算总体积（不信 Metadata）。
    pub fn total_volume(&self) -> u64 {
        self.regions.iter().map(|r| r.volume()).sum()
    }

    /// 实算非空气方块数。需要 `LoadMode::Full`。
    pub fn total_blocks(&self) -> Option<u64> {
        if !self.blocks_loaded {
            return None;
        }
        let mut total = 0u64;
        for r in &self.regions {
            let hist = r.palette_histogram();
            for (i, n) in hist.iter().enumerate() {
                if let Some(bs) = r.palette.get(i) {
                    if !bs.is_air() {
                        total += n;
                    }
                }
            }
        }
        Some(total)
    }

    /// 蓝图整体包围盒（局部坐标）。多 region 时是所有 region 的并集。
    pub fn bounding_box(&self) -> Option<(Vec3i, Vec3i)> {
        let mut min: Option<Vec3i> = None;
        let mut max: Option<Vec3i> = None;
        for r in &self.regions {
            let lo = r.min_corner();
            let e = r.extent();
            let hi = Vec3i { x: lo.x + e.x - 1, y: lo.y + e.y - 1, z: lo.z + e.z - 1 };
            min = Some(match min {
                None => lo,
                Some(m) => Vec3i { x: m.x.min(lo.x), y: m.y.min(lo.y), z: m.z.min(lo.z) },
            });
            max = Some(match max {
                None => hi,
                Some(m) => Vec3i { x: m.x.max(hi.x), y: m.y.max(hi.y), z: m.z.max(hi.z) },
            });
        }
        Some((min?, max?))
    }

    /// 声明值与实算值是否对得上。对不上说明 Metadata 被改过。
    pub fn metadata_is_truthful(&self) -> bool {
        match self.metadata.declared_total_volume {
            Some(d) => d as u64 == self.total_volume(),
            None => true,
        }
    }
}

fn parse_metadata(c: &Compound) -> Metadata {
    Metadata {
        name: c.get("Name").and_then(Value::as_str).map(str::to_owned),
        author: c.get("Author").and_then(Value::as_str).map(str::to_owned),
        description: c.get("Description").and_then(Value::as_str).map(str::to_owned),
        time_created: c.get("TimeCreated").and_then(Value::as_i64),
        time_modified: c.get("TimeModified").and_then(Value::as_i64),
        declared_total_blocks: c.get("TotalBlocks").and_then(Value::as_i64),
        declared_total_volume: c.get("TotalVolume").and_then(Value::as_i64),
        declared_region_count: c.get("RegionCount").and_then(Value::as_i32),
        enclosing_size: c.get("EnclosingSize").and_then(Value::as_compound).and_then(Vec3i::from_nbt),
        preview_image_argb: match c.get("PreviewImageData") {
            Some(Value::IntArray(v)) => Some(v.clone()),
            _ => None,
        },
    }
}

fn parse_region(name: String, c: &Compound) -> Result<Region> {
    let position = c
        .get("Position")
        .and_then(Value::as_compound)
        .and_then(Vec3i::from_nbt)
        .unwrap_or(Vec3i { x: 0, y: 0, z: 0 });
    let size = c
        .get("Size")
        .and_then(Value::as_compound)
        .and_then(Vec3i::from_nbt)
        .ok_or_else(|| anyhow!("region {name} 缺少 Size"))?;

    let palette = c
        .get("BlockStatePalette")
        .and_then(Value::as_list)
        .map(|list| list.iter().filter_map(parse_block_state).collect::<Vec<_>>())
        .unwrap_or_default();

    let block_states = c
        .get("BlockStates")
        .and_then(Value::as_long_array)
        .map(|s| s.to_vec())
        .unwrap_or_default();

    let tile_entities = c
        .get("TileEntities")
        .and_then(Value::as_list)
        .map(|list| list.iter().filter_map(parse_tile_entity).collect::<Vec<_>>())
        .unwrap_or_default();

    let entity_count = c.get("Entities").and_then(Value::as_list).map_or(0, <[Value]>::len);

    Ok(Region { name, position, size, palette, block_states, tile_entities, entity_count })
}

fn parse_block_state(v: &Value) -> Option<BlockState> {
    let c = v.as_compound()?;
    let name = c.get("Name")?.as_str()?.to_owned();
    let mut properties = BTreeMap::new();
    if let Some(props) = c.get("Properties").and_then(Value::as_compound) {
        for (k, pv) in props {
            if let Some(s) = pv.as_str() {
                properties.insert(k.clone(), s.to_owned());
            }
        }
    }
    Some(BlockState { name, properties })
}

fn parse_tile_entity(v: &Value) -> Option<TileEntity> {
    let c = v.as_compound()?;
    let pos = Vec3i {
        x: c.get("x").and_then(Value::as_i32).unwrap_or(0),
        y: c.get("y").and_then(Value::as_i32).unwrap_or(0),
        z: c.get("z").and_then(Value::as_i32).unwrap_or(0),
    };
    let id = c.get("id").and_then(Value::as_str).map(str::to_owned);
    let items = c
        .get("Items")
        .and_then(Value::as_list)
        .map(|list| list.iter().filter_map(parse_item_stack).collect::<Vec<_>>())
        .unwrap_or_default();
    Some(TileEntity { pos, id, items })
}

fn parse_item_stack(v: &Value) -> Option<ItemStack> {
    let c = v.as_compound()?;
    let id = c.get("id")?.as_str()?.to_owned();
    // 1.20.5 起数量字段从 `Count`(byte) 改成了 `count`(int)。
    // 本地蓝图 DataVersion 从 1631(1.13) 到 4189(1.21.4) 都有，两种都得认。
    let count = c
        .get("Count")
        .or_else(|| c.get("count"))
        .and_then(Value::as_i64)
        .unwrap_or(1);
    Some(ItemStack { id, count })
}
