//! Schemadex 后端命令。
//!
//! 设计要点：
//! - **扫描分两段**：先用索引模式（跳过 BlockStates）拿结构信息，2138 个文件 0.6 秒；
//!   缩略图和材料清单才走全量解析，且按需触发。
//! - **去重按内容哈希**：同一蓝图在多个版本目录各存一份是常态（实测 64% 冗余），
//!   只按文件名或体积去重会误判。
//! - **缩略图落盘缓存**：键是内容哈希，所以副本天然共享同一张图。

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base64::Engine;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use tauri::{Manager, State};

use litematic::materials::{stack_size, to_stacks, MaterialOptions};
use litematic::{LoadMode, MaterialList, Schematic};
use mcassets::BlockAssets;

/// 材质表与图集**不编进二进制**，首次运行时从用户自己的 Minecraft 客户端 jar 生成，
/// 存到应用数据目录。
///
/// 这两份东西是从 `assets/minecraft/textures/block/`、`models/`、`lang/zh_cn.json`
/// 提取出来的——**是 Mojang 的素材，不能随程序分发**。以前是 `include_bytes!`
/// 编进 exe 的，那样连发一个 release 二进制都不合规。
/// 现在换成「用户拿自己已经装好的游戏生成一份」，谁装了 MC 谁就有素材，
/// 我们既不复制也不传播。
///
/// 生成逻辑就是 `schemadex colors <jar>` 那一条，见 [`build_assets`]。
const ASSETS_DIR: &str = "assets";
const ASSETS_JSON: &str = "colors.json";
const ASSETS_PNG: &str = "colors.png";

/// 渲染器版本。**渲染方式一改就把这个数 +1**。
///
/// 缩略图的磁盘缓存键是蓝图内容哈希、不含渲染器版本，所以换了渲染方式却不换
/// 缓存目录的话，用户会一直看到旧渲染器出的图。缓存目录名和启动时要清理的旧目录
/// 列表**都由这个常量算出来**——以前是两处各写一遍，改一处漏一处就出事。
///
/// v1: 平均色填充；v2: 真材质 + 超采样 + 描边；
/// v3: 材质改为按 model 的 elements/faces + variant 旋转解析；
/// v4: 按 model 的实际包围盒渲染，不再把火把/活板门/栅栏画成完整立方体；
/// v5: 按真实方块状态解析模型（朝向、上下半格、楼梯两段、栅栏横杆）；
/// v6: 读 face 的显式 uv、方块实体用实体贴图、重叠 element 去重；
/// v7: 图集保留 alpha 改为绘制时混合、面内纹理旋转（红石线走向）；
/// v8: 模型没声明的面不再拿别的面顶上（红石线的游离小红点、红石火把糊成一团）；
/// v9: 降采样的体素改贴代表图块（以前大蓝图整张缩略图都是色块）、
///     包围盒允许伸出格子外（活塞头的杆不再和本体断开）。
///
/// 判断标准是**等距那条路的输出变没变**，不是「有没有动 render crate」。
/// 「玻璃不再遮挡邻居」那次就没有 +1：它修的是 `surface_voxels`（只喂 3D 视图），
/// 等距渲染走的是「所有实心格子由远及近全画一遍」，压根不查遮挡，出图逐像素不变。
/// v8 这次动的是 element 的面，等距出图确实变了，才要 +1。
/// 白 +1 的代价是让用户重新生成两千多张缩略图。
const RENDERER_VERSION: u32 = 9;

fn thumb_cache_dir() -> String {
    format!("thumbs-v{RENDERER_VERSION}")
}

// ---------------------------------------------------------------- 数据结构

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Blueprint {
    /// 内容哈希，同时用作缩略图缓存键
    pub id: String,
    /// 代表路径（副本里的第一个）
    pub path: String,
    /// 其余同内容副本的路径
    pub duplicates: Vec<String>,
    pub file_name: String,
    /// Metadata 里的名字，"Unnamed" 视为无
    pub name: Option<String>,
    pub author: Option<String>,
    /// 包围盒尺寸 [x, y, z]
    pub size: [i32; 3],
    pub volume: u64,
    pub region_count: usize,
    pub data_version: i32,
    pub file_size: u64,
    /// 文件修改时间（Unix 秒）
    pub modified: Option<i64>,
    /// Metadata 的声明值与实算值是否一致；false 说明这个蓝图的 Metadata 被改过
    pub metadata_trustworthy: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanResult {
    pub blueprints: Vec<Blueprint>,
    pub total_files: usize,
    pub unique: usize,
    pub failed: Vec<FailedFile>,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FailedFile {
    pub path: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MaterialRow {
    pub item: String,
    pub count: u64,
    pub boxes: u64,
    pub stacks: u64,
    pub rest: u64,
    pub stack_size: u64,
    /// 该项用了近似映射，数字需人工复核
    pub inexact: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct MaterialsResult {
    pub rows: Vec<MaterialRow>,
    pub container_items: Vec<MaterialRow>,
    pub total_blocks: u64,
    pub total_volume: u64,
    pub blueprint_count: usize,
    /// 解析失败的蓝图，避免汇总数字悄悄少算
    pub failed: Vec<FailedFile>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VoxelPaletteEntry {
    /// 完整命名空间 ID，悬停提示直接用它
    pub name: String,
    pub top: [u8; 3],
    pub side: [u8; 3],
    /// 中文名，没有译名时为 None
    pub label: Option<String>,
    /// 按真实方块状态解析出的长方体。楼梯两段、栅栏柱子加横杆都在这里；
    /// 空表示这个方块没有模型（箱子等方块实体），前端画一个平均色立方体。
    pub boxes: Vec<mcassets::ResolvedBox>,
    /// `opaque` / `cutout` / `translucent`。前端据此分两趟画：
    /// 半透明的必须开 alpha 混合、关深度写入，alphaTest 对染色玻璃完全无效。
    pub opacity: render::Opacity,
    /// 降采样时的代表图块 `[顶面, 侧面]`。scale > 1 时一个体素按整格画，
    /// 六面贴这两张——退回平均色的话大蓝图会整个变成色块。
    pub repr: [u16; 2],
}

#[derive(Debug, Clone, Serialize)]
pub struct VoxelModel {
    /// 体素网格尺寸 [x, y, z]
    pub dims: [u32; 3],
    /// 一个体素代表原蓝图多少个方块（每轴）。>1 表示做了降采样
    pub scale: u32,
    pub palette: Vec<VoxelPaletteEntry>,
    /// base64。解开后每个体素 8 字节：小端 u16 的 x, y, z, 调色板索引
    pub data: String,
    pub count: usize,
    /// 因为体素数超上限而进一步降了精度
    pub reduced: bool,
}

/// 方块图集，给前端 WebGL 做纹理。
#[derive(Debug, Clone, Serialize)]
pub struct AtlasInfo {
    pub tile_size: u32,
    pub tiles_per_row: u32,
    /// data: URL 形式的 PNG
    pub image: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BlueprintDetail {
    pub layers: usize,
    /// 每层的非空气方块数，用来在切片滑块上标出哪几层有料
    pub layer_counts: Vec<u64>,
    pub palette_size: usize,
    /// 出现最多的方块，前 20
    pub top_blocks: Vec<(String, u64)>,
}

// ---------------------------------------------------------------- 应用状态

/// 一套已经就绪的材质数据。
pub struct Assets {
    colors: BlockAssets,
    /// 方块图集。解码失败就退回平均色渲染，不至于整个应用起不来
    atlas: Option<render::Atlas>,
    /// 图集 PNG 原始字节，前端要拿它当纹理
    png: Vec<u8>,
    /// 生成时用的 MC 版本，界面上显示给用户看
    version: String,
}

pub struct AppState {
    /// 没配置材质表时是 None——**应用要能在这个状态下正常启动**，
    /// 引导用户去指一个客户端 jar，而不是直接 panic
    assets: Mutex<Option<Assets>>,
    /// 路径 → 缓存的索引结果
    index: Mutex<BTreeMap<String, Blueprint>>,
    cache_dir: Mutex<Option<PathBuf>>,
}

impl AppState {
    fn new() -> Self {
        Self {
            assets: Mutex::new(None),
            index: Mutex::new(BTreeMap::new()),
            cache_dir: Mutex::new(None),
        }
    }

    fn thumb_path(&self, id: &str) -> Option<PathBuf> {
        let dir = self.cache_dir.lock().ok()?.clone()?;
        Some(dir.join(thumb_cache_dir()).join(format!("{id}.png")))
    }

    fn assets_dir(&self) -> Option<PathBuf> {
        Some(self.cache_dir.lock().ok()?.clone()?.join(ASSETS_DIR))
    }

    /// 借出材质表跑一段渲染。没配置时给一句能看懂的错，而不是 unwrap 崩掉。
    fn with_assets<T>(&self, f: impl FnOnce(&Assets) -> Result<T, String>) -> Result<T, String> {
        let guard = self.assets.lock().map_err(|_| "材质表状态被污染了".to_string())?;
        let a = guard
            .as_ref()
            .ok_or_else(|| "还没生成材质表。先在设置里指定一个 Minecraft 客户端 jar。".to_string())?;
        f(a)
    }
}

/// 从磁盘加载之前生成好的材质表。
fn load_assets(dir: &Path) -> Option<Assets> {
    let json = fs::read_to_string(dir.join(ASSETS_JSON)).ok()?;
    let colors: BlockAssets = serde_json::from_str(&json).ok()?;
    let png = fs::read(dir.join(ASSETS_PNG)).ok()?;
    let atlas =
        render::Atlas::from_png(&png, colors.tile_size.max(1), colors.tiles_per_row.max(1)).ok();
    let version = colors.version.clone();
    Some(Assets { colors, atlas, png, version })
}

// ---------------------------------------------------------------- 扫描

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() => walk(&p, out),
            Ok(t) if t.is_file() => {
                if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("litematic")) {
                    out.push(p);
                }
            }
            _ => {}
        }
    }
}

fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut f = fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn index_one(path: &Path) -> Result<Blueprint, String> {
    let schem = Schematic::load(path, LoadMode::Index).map_err(|e| format!("{e:#}"))?;
    let (lo, hi) = schem
        .bounding_box()
        .ok_or_else(|| "蓝图没有任何 region".to_string())?;
    let meta = fs::metadata(path).map_err(|e| e.to_string())?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);

    let id = hash_file(path).map_err(|e| e.to_string())?;

    // 先把要借用 schem 的都算完，再去 move 出 metadata 里的字段
    let volume = schem.total_volume();
    let region_count = schem.regions.len();
    let data_version = schem.data_version;
    let metadata_trustworthy = schem.metadata_is_truthful();

    // Metadata.Name 有 73% 是 "Unnamed"，这种情况下文件名才是唯一有效信息
    let name = schem
        .metadata
        .name
        .filter(|n| !n.is_empty() && n != "Unnamed");
    let author = schem.metadata.author.filter(|a| !a.is_empty());

    Ok(Blueprint {
        id,
        path: path.to_string_lossy().into_owned(),
        duplicates: Vec::new(),
        file_name: path.file_name().unwrap_or_default().to_string_lossy().into_owned(),
        name,
        author,
        size: [hi.x - lo.x + 1, hi.y - lo.y + 1, hi.z - lo.z + 1],
        volume,
        region_count,
        data_version,
        file_size: meta.len(),
        modified,
        metadata_trustworthy,
    })
}

#[tauri::command]
fn scan(roots: Vec<String>, state: State<AppState>) -> ScanResult {
    let t0 = std::time::Instant::now();
    let mut files = Vec::new();
    for r in &roots {
        walk(Path::new(r), &mut files);
    }
    files.sort();
    files.dedup();
    let total_files = files.len();

    let results: Vec<(PathBuf, Result<Blueprint, String>)> = files
        .par_iter()
        .map(|p| (p.clone(), index_one(p)))
        .collect();

    let mut by_hash: BTreeMap<String, Blueprint> = BTreeMap::new();
    let mut failed = Vec::new();
    for (p, r) in results {
        match r {
            Ok(bp) => match by_hash.get_mut(&bp.id) {
                // 同内容的副本收进 duplicates，界面上只出现一次
                Some(existing) => existing.duplicates.push(bp.path),
                None => {
                    by_hash.insert(bp.id.clone(), bp);
                }
            },
            Err(e) => failed.push(FailedFile { path: p.to_string_lossy().into_owned(), error: e }),
        }
    }

    let mut blueprints: Vec<Blueprint> = by_hash.into_values().collect();
    blueprints.sort_by(|a, b| a.file_name.cmp(&b.file_name));

    if let Ok(mut idx) = state.index.lock() {
        idx.clear();
        for bp in &blueprints {
            idx.insert(bp.path.clone(), bp.clone());
            for d in &bp.duplicates {
                idx.insert(d.clone(), bp.clone());
            }
        }
    }

    ScanResult {
        unique: blueprints.len(),
        blueprints,
        total_files,
        failed,
        elapsed_ms: t0.elapsed().as_millis() as u64,
    }
}

// ---------------------------------------------------------------- 缩略图

fn png_data_url(img: &image::RgbaImage) -> Result<String, String> {
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(buf.into_inner())
    ))
}

#[tauri::command]
fn thumbnail(path: String, id: Option<String>, state: State<AppState>) -> Result<String, String> {
    // 缓存键是内容哈希，所以 4 份副本只渲染一次
    let cached = id.as_ref().and_then(|i| state.thumb_path(i));
    if let Some(p) = &cached {
        if let Ok(bytes) = fs::read(p) {
            return Ok(format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(bytes)
            ));
        }
    }

    let schem = Schematic::load(&path, LoadMode::Full).map_err(|e| format!("{e:#}"))?;
    let opts = render::RenderOptions { target_grid: 128, max_px: 400, ..Default::default() };
    let img = state.with_assets(|a| {
        render::isometric(&schem, &a.colors, a.atlas.as_ref(), &opts)
            .map_err(|e| format!("{e:#}"))
    })?;

    if let Some(p) = &cached {
        if let Some(parent) = p.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = img.save(p);
    }
    png_data_url(&img)
}

#[tauri::command]
fn slice(path: String, y: usize, cell_px: u32, state: State<AppState>) -> Result<String, String> {
    let schem = Schematic::load(&path, LoadMode::Full).map_err(|e| format!("{e:#}"))?;
    let opts = render::RenderOptions {
        background: Some([0x18, 0x18, 0x1b]),
        ..Default::default()
    };
    let img = state.with_assets(|a| {
        render::slice_top_down(&schem, &a.colors, a.atlas.as_ref(), y, cell_px.clamp(1, 32), &opts)
            .map_err(|e| format!("{e:#}"))
    })?;
    png_data_url(&img)
}

#[tauri::command]
fn detail(path: String) -> Result<BlueprintDetail, String> {
    let schem = Schematic::load(&path, LoadMode::Full).map_err(|e| format!("{e:#}"))?;
    // 内存 O(层数)，不建完整 3D 网格
    let layer_counts =
        render::layer_counts(&schem).ok_or_else(|| "蓝图没有 region".to_string())?;
    let layers = layer_counts.len();

    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut palette_size = 0usize;
    for r in &schem.regions {
        palette_size += r.palette.len();
        let hist = r.palette_histogram();
        for (i, n) in hist.iter().enumerate() {
            if *n == 0 {
                continue;
            }
            if let Some(bs) = r.palette.get(i) {
                if !bs.is_air() {
                    *counts.entry(bs.name.clone()).or_insert(0) += n;
                }
            }
        }
    }
    let mut top_blocks: Vec<(String, u64)> = counts.into_iter().collect();
    top_blocks.sort_by(|a, b| b.1.cmp(&a.1));
    top_blocks.truncate(20);

    Ok(BlueprintDetail { layers, layer_counts, palette_size, top_blocks })
}

/// 前端实际要画的长方体总数。跟 `Viewer3D` 里数 instance 的口径必须一致：
/// 降采样时统一按一个完整立方体画，否则每个方块按它的模型展开。
fn instance_count(grid: &render::VoxelGrid, surface: &[(u16, u16, u16, u16)]) -> usize {
    if grid.scale > 1 {
        return surface.len();
    }
    surface
        .iter()
        .map(|(_, _, _, bi)| grid.palette.get(*bi as usize).map_or(1, |b| b.boxes.len().max(1)))
        .sum()
}

/// 前端 WebGL 用的表面体素模型。
///
/// 只送六邻接里有裸露面的体素——内部的谁也看不见，剔掉之后实心建筑的
/// 体素数从 O(n³) 掉到 O(n²)，才可能把整个模型丢给浏览器去渲染。
#[tauri::command]
fn voxels(path: String, max_grid: u32, state: State<AppState>) -> Result<VoxelModel, String> {
    let schem = Schematic::load(&path, LoadMode::Full).map_err(|e| format!("{e:#}"))?;

    // 画得太多会拖垮 WebGL 的 instanced 渲染，也会让 IPC 传输变慢。
    // 超了就降一档精度重来。
    //
    // **上限卡在 instance 上，不是体素上**：前端一个长方体一个 instance，
    // 一个栅栏方块就展开成 5 个。只卡体素数的话，栅栏/玻璃密集的蓝图
    // 体素数没超、instance 数已经翻了好几倍，照样卡死。
    const MAX_INSTANCES: usize = 900_000;
    state.with_assets(|a| {
    let mut target = max_grid.clamp(32, 256);
    let mut reduced = false;
    let (grid, surface) = loop {
        let opts = render::RenderOptions { target_grid: target, ..Default::default() };
        let grid = render::VoxelGrid::build(&schem, &a.colors, a.atlas.as_ref(), &opts)
            .ok_or_else(|| "蓝图没有 region".to_string())?;
        let surface = grid.surface_voxels();
        let instances = instance_count(&grid, &surface);
        if instances <= MAX_INSTANCES || target <= 48 {
            break (grid, surface);
        }
        // 表面积约与边长平方成正比，按比例回退一档
        let shrink = ((MAX_INSTANCES as f64 / instances as f64).sqrt() * 0.95).clamp(0.4, 0.9);
        target = ((target as f64 * shrink) as u32).max(48);
        reduced = true;
    };

    let mut bytes = Vec::with_capacity(surface.len() * 8);
    for (x, y, z, bi) in &surface {
        bytes.extend_from_slice(&x.to_le_bytes());
        bytes.extend_from_slice(&y.to_le_bytes());
        bytes.extend_from_slice(&z.to_le_bytes());
        bytes.extend_from_slice(&bi.to_le_bytes());
    }

    Ok(VoxelModel {
        dims: [grid.w as u32, grid.h as u32, grid.d as u32],
        scale: grid.scale,
        palette: grid
            .palette
            .iter()
            .map(|b| VoxelPaletteEntry {
                name: b.name.clone(),
                top: b.top,
                side: b.side,
                label: a.colors.name_of(&b.name).map(str::to_owned),
                boxes: b.boxes.clone(),
                opacity: b.opacity,
                repr: [b.repr.0, b.repr.1],
            })
            .collect(),
        count: surface.len(),
        data: base64::engine::general_purpose::STANDARD.encode(&bytes),
        reduced,
    })
    })
}

/// 方块与物品的中文名。前端整个会话只取一次。没配置材质表时返回空表。
#[tauri::command]
fn names(state: State<AppState>) -> BTreeMap<String, String> {
    state.with_assets(|a| Ok(a.colors.names.clone())).unwrap_or_default()
}

/// 图集本体。前端拿去当纹理，整个会话只取一次。
#[tauri::command]
fn atlas(state: State<AppState>) -> Result<AtlasInfo, String> {
    state.with_assets(|a| {
        Ok(AtlasInfo {
            tile_size: a.colors.tile_size.max(1),
            tiles_per_row: a.colors.tiles_per_row.max(1),
            image: format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(&a.png)
            ),
        })
    })
}

// ---------------------------------------------------------------- 材料汇总

fn to_rows(map: &BTreeMap<String, u64>, inexact: &std::collections::BTreeSet<String>) -> Vec<MaterialRow> {
    let mut rows: Vec<MaterialRow> = map
        .iter()
        .map(|(item, &count)| {
            let ss = stack_size(item);
            let (boxes, stacks, rest) = to_stacks(count, ss);
            MaterialRow {
                item: item.clone(),
                count,
                boxes,
                stacks,
                rest,
                stack_size: ss,
                inexact: inexact.contains(item),
            }
        })
        .collect();
    rows.sort_by(|a, b| b.count.cmp(&a.count).then(a.item.cmp(&b.item)));
    rows
}

#[tauri::command]
fn materials(paths: Vec<String>, count_fluids: bool) -> MaterialsResult {
    let opts = MaterialOptions { count_fluids, count_container_items: true };

    let parts: Vec<Result<MaterialList, FailedFile>> = paths
        .par_iter()
        .map(|p| match Schematic::load(p, LoadMode::Full) {
            Ok(s) => Ok(MaterialList::of(&s, &opts)),
            Err(e) => Err(FailedFile { path: p.clone(), error: format!("{e:#}") }),
        })
        .collect();

    let mut merged = MaterialList::default();
    let mut failed = Vec::new();
    let mut ok = 0usize;
    for r in parts {
        match r {
            Ok(m) => {
                merged.merge(&m);
                ok += 1;
            }
            Err(f) => failed.push(f),
        }
    }

    MaterialsResult {
        rows: to_rows(&merged.blocks, &merged.inexact),
        container_items: to_rows(&merged.container_items, &Default::default()),
        total_blocks: merged.total_blocks,
        total_volume: merged.total_volume,
        blueprint_count: ok,
        failed,
    }
}

/// 猜一下蓝图目录在哪。国内玩家多用 PCL/HMCL，版本隔离会让蓝图散在
/// `versions/<各版本>/schematics/` 下，而不是官方启动器的单一目录。
// ---------------------------------------------------------------- 材质表配置

#[derive(Debug, Clone, Serialize)]
pub struct AssetsStatus {
    /// 材质表是否就绪。false 时前端要挡在引导页，别让用户点进去看空白
    pub ready: bool,
    /// 生成时用的 MC 版本
    pub version: Option<String>,
    /// 存放位置，界面上显示出来便于排查
    pub dir: Option<String>,
}

#[tauri::command]
fn assets_status(state: State<AppState>) -> AssetsStatus {
    let dir = state.assets_dir().map(|p| p.to_string_lossy().into_owned());
    match state.assets.lock() {
        Ok(g) => match g.as_ref() {
            Some(a) => AssetsStatus { ready: true, version: Some(a.version.clone()), dir },
            None => AssetsStatus { ready: false, version: None, dir },
        },
        Err(_) => AssetsStatus { ready: false, version: None, dir },
    }
}

/// 找出机器上可能的客户端 jar，供用户挑。
///
/// 常见布局是 `.minecraft/versions/<版本>/<版本>.jar`，但**别只认这一种**——
/// 实测有把 1.21.4 直接放在 `.minecraft/1.21.4/1.21.4.jar` 的。
/// 所以在每个 `.minecraft` 下面扫两层，凡是 `<目录名>.jar` 就算候选。
#[tauri::command]
fn suggest_jars() -> Vec<String> {
    let mut out = Vec::new();
    for root in suggest_roots() {
        let root = PathBuf::from(root);
        let mut dirs = vec![root.clone()];
        if let Ok(rd) = fs::read_dir(root.join("versions")) {
            dirs.extend(rd.flatten().map(|e| e.path()));
        }
        if let Ok(rd) = fs::read_dir(&root) {
            dirs.extend(rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
        }
        for d in dirs {
            let Some(name) = d.file_name().map(|n| n.to_string_lossy().into_owned()) else {
                continue;
            };
            let jar = d.join(format!("{name}.jar"));
            if jar.is_file() {
                out.push(jar.to_string_lossy().into_owned());
            }
        }
    }
    out.sort();
    out.dedup();
    // **新版本排前面。** 默认按字母序的话 1.19.2 会排在 1.21.4 前头，
    // 用户十有八九点第一个——然后 1.19.2 之后加的方块（樱花木、幽匿、铜灯、
    // 合成器、苍白橡木…）全部退化成兜底色，而且没有任何提示。
    out.sort_by(|a, b| version_key(b).cmp(&version_key(a)));
    out
}

/// 从路径里抽出版本号用于排序，如 `1.21.4-Fabric` → `[1, 21, 4]`。
/// 抽不出数字的排最后。**必须按数字比，不能按字典序**：字典序下 "1.9" > "1.21"。
fn version_key(path: &str) -> Vec<u32> {
    let name = Path::new(path).file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in name.chars() {
        if c.is_ascii_digit() {
            cur.push(c);
        } else if !cur.is_empty() {
            out.push(cur.parse().unwrap_or(0));
            cur.clear();
        }
    }
    if !cur.is_empty() {
        out.push(cur.parse().unwrap_or(0));
    }
    out
}

/// 从客户端 jar 生成材质表，写进应用数据目录并立刻装载。
///
/// 就是 `schemadex colors` 那条命令的应用内版本。**素材始终来自用户自己的游戏**，
/// 我们不分发任何 Mojang 的东西。
#[tauri::command]
fn build_assets(jar: String, state: State<AppState>) -> Result<AssetsStatus, String> {
    let dir = state.assets_dir().ok_or_else(|| "拿不到应用数据目录".to_string())?;
    fs::create_dir_all(&dir).map_err(|e| format!("建不了 {}: {e}", dir.display()))?;

    let extracted = mcassets::extract(Path::new(&jar)).map_err(|e| format!("{e:#}"))?;
    let json = serde_json::to_string(&extracted.assets).map_err(|e| e.to_string())?;
    fs::write(dir.join(ASSETS_JSON), &json).map_err(|e| e.to_string())?;
    fs::write(dir.join(ASSETS_PNG), &extracted.atlas_png).map_err(|e| e.to_string())?;

    let loaded = load_assets(&dir).ok_or_else(|| "生成完却读不回来".to_string())?;
    let status =
        AssetsStatus {
            ready: true,
            version: Some(loaded.version.clone()),
            dir: Some(dir.to_string_lossy().into_owned()),
        };
    *state.assets.lock().map_err(|_| "材质表状态被污染了".to_string())? = Some(loaded);

    // 换了材质表，旧缩略图就都过期了
    if let Some(c) = state.cache_dir.lock().ok().and_then(|g| g.clone()) {
        let _ = fs::remove_dir_all(c.join(thumb_cache_dir()));
        let _ = fs::create_dir_all(c.join(thumb_cache_dir()));
    }
    if let Ok(mut idx) = state.index.lock() {
        idx.clear();
    }
    Ok(status)
}

#[tauri::command]
fn suggest_roots() -> Vec<String> {
    let mut out = Vec::new();
    let mut push_if_exists = |p: PathBuf| {
        if p.is_dir() {
            out.push(p.to_string_lossy().into_owned());
        }
    };
    if let Ok(appdata) = std::env::var("APPDATA") {
        push_if_exists(PathBuf::from(&appdata).join(".minecraft"));
    }
    for drive in ["C:\\", "D:\\", "E:\\", "F:\\"] {
        let root = Path::new(drive);
        let Ok(rd) = fs::read_dir(root) else { continue };
        for e in rd.flatten().take(200) {
            let p = e.path().join(".minecraft");
            if p.is_dir() {
                out.push(p.to_string_lossy().into_owned());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::new())
        .setup(|app| {
            let dir = app.path().app_cache_dir().ok();
            if let Some(d) = &dir {
                let _ = fs::create_dir_all(d.join(thumb_cache_dir()));
                // 顺手清掉旧版渲染器留下的缓存，别白占几百 MB。
                // 名单由 RENDERER_VERSION 推出来，不用手工跟着加
                let _ = fs::remove_dir_all(d.join("thumbs"));
                for v in 1..RENDERER_VERSION {
                    let _ = fs::remove_dir_all(d.join(format!("thumbs-v{v}")));
                }
            }
            if let Some(state) = app.try_state::<AppState>() {
                if let Ok(mut c) = state.cache_dir.lock() {
                    *c = dir;
                }
                // 之前生成过就直接装载；没有就保持 None，前端会引导用户去生成
                if let Some(a) = state.assets_dir().as_deref().and_then(load_assets) {
                    if let Ok(mut g) = state.assets.lock() {
                        *g = Some(a);
                    }
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            scan,
            thumbnail,
            slice,
            detail,
            voxels,
            atlas,
            names,
            materials,
            suggest_roots,
            assets_status,
            suggest_jars,
            build_assets
        ])
        .run(tauri::generate_context!())
        .expect("Tauri 启动失败");
}

#[cfg(test)]
mod tests {
    use super::version_key;

    /// 版本号必须按数字比。字典序下 "1.9" 会排到 "1.21" 前面，
    /// 结果用户默认拿到的是最旧的客户端。
    #[test]
    fn newer_versions_sort_first() {
        let mut v = vec![
            r"D:\mc\versions\1.19.2-Fabric 0.15.10\1.19.2-Fabric 0.15.10.jar".to_string(),
            r"D:\mc\1.21.4\1.21.4.jar".to_string(),
            r"D:\mc\versions\1.9.4\1.9.4.jar".to_string(),
            r"D:\mc\versions\1.20.1\1.20.1.jar".to_string(),
        ];
        v.sort_by(|a, b| version_key(b).cmp(&version_key(a)));
        assert!(v[0].contains("1.21.4"), "最新的该排第一，实际 {:?}", v[0]);
        assert!(v[1].contains("1.20.1"));
        assert!(v[2].contains("1.19.2"));
        assert!(v[3].contains("1.9.4"), "1.9 该排最后，字典序会把它排前面");
    }
}
