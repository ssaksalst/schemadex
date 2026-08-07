//! 从 Minecraft 客户端 jar 提取方块模型、材质图集与中文名。
//!
//! 走游戏自己的解析链，不猜：
//!
//! ```text
//! assets/minecraft/blockstates/<block>.json  → variants / multipart 规则
//! assets/minecraft/models/block/<model>.json → parent 链合并 textures + elements
//! assets/minecraft/textures/block/<tex>.png  → 图块本体 + 平均色
//! assets/minecraft/lang/zh_cn.json           → 中文名
//! ```
//!
//! **整套规则原样导出**，不在这里挑代表 variant——蓝图里的方块带着
//! `facing` / `half` / `axis` 等状态，必须留到运行时按真实状态解析，
//! 否则所有活塞都朝上、所有活板门都在下半格、楼梯只有一段、栅栏没有横杆。

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{Cursor, Read};
use std::path::Path;

use anyhow::{Context, Result};
use image::{ImageEncoder, RgbaImage};

use crate::model::{self, BlockDef, ElementDef, ModelDef, PartDef, VariantDef};
use crate::{BlockAssets, BlockColor, Extracted, Rgb, NO_FACE, NO_TILE};

const TILE: u32 = 16;

/// 已解压到内存的 jar。
struct Jar {
    files: HashMap<String, Vec<u8>>,
}

impl Jar {
    fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("打不开 jar {}", path.display()))?;
        let mut zip = zip::ZipArchive::new(file).context("jar 不是合法 zip")?;
        let mut files = HashMap::new();
        for i in 0..zip.len() {
            let mut e = zip.by_index(i)?;
            if !e.is_file() {
                continue;
            }
            let name = e.name().to_string();
            // 只留用得上的资源，避免把 20MB 的 class 文件全读进来
            let keep = name.starts_with("assets/minecraft/blockstates/")
                || name.starts_with("assets/minecraft/models/")
                || name.starts_with("assets/minecraft/textures/block/")
                || name.starts_with("assets/minecraft/textures/entity/")
                || name == "assets/minecraft/lang/zh_cn.json"
                || name == "version.json";
            if !keep {
                continue;
            }
            let mut buf = Vec::with_capacity(e.size() as usize);
            e.read_to_end(&mut buf)?;
            files.insert(name, buf);
        }
        Ok(Self { files })
    }

    fn json(&self, path: &str) -> Option<serde_json::Value> {
        serde_json::from_slice(self.files.get(path)?).ok()
    }
}

/// `minecraft:block/stone` / `block/stone` → `block/stone`
fn strip_ns(s: &str) -> &str {
    s.split_once(':').map_or(s, |(_, rest)| rest)
}

// ---------------------------------------------------------------- 生物群系染色

/// 这些方块的贴图本身是灰度的，游戏里按群系着色，直接用会得到一片惨白。
/// 用平原群系的色值去乘。
fn biome_tint(block: &str) -> Option<Rgb> {
    let id = block.strip_prefix("minecraft:")?;
    match id {
        "spruce_leaves" => return Some([0x61, 0x99, 0x61]),
        "birch_leaves" => return Some([0x80, 0xA7, 0x55]),
        // 这几种叶子本身就是彩色贴图，不染色
        "cherry_leaves" | "azalea_leaves" | "flowering_azalea_leaves" | "pale_oak_leaves" => {
            return None
        }
        _ => {}
    }
    const GRASS: Rgb = [0x91, 0xBD, 0x59];
    const FOLIAGE: Rgb = [0x77, 0xAB, 0x2F];
    const WATER: Rgb = [0x3F, 0x76, 0xE4];
    Some(match id {
        "grass_block" | "short_grass" | "grass" | "tall_grass" | "fern" | "large_fern"
        | "potted_fern" | "sugar_cane" => GRASS,
        "vine" | "lily_pad" => FOLIAGE,
        "water" | "water_cauldron" | "bubble_column" => WATER,
        // 红石线未通电时是暗红，贴图接近灰度
        "redstone_wire" => [0x8B, 0x00, 0x00],
        _ if id.ends_with("_leaves") => FOLIAGE,
        _ => return None,
    })
}

fn apply_tint(c: Rgb, tint: Rgb) -> Rgb {
    [
        ((c[0] as u32 * tint[0] as u32) / 255) as u8,
        ((c[1] as u32 * tint[1] as u32) / 255) as u8,
        ((c[2] as u32 * tint[2] as u32) / 255) as u8,
    ]
}

// ---------------------------------------------------------------- 模型解析

struct ResolvedModel {
    textures: BTreeMap<String, String>,
    elements: Option<Vec<serde_json::Value>>,
}

/// 沿 parent 链解析模型。
///
/// `textures` 逐层合并、子覆盖父；`elements` 则是**整体覆盖**——
/// 一旦某层定义了 elements，父层的几何就完全不用了（MC 自己就这么做）。
fn resolve_model(jar: &Jar, model: &str) -> ResolvedModel {
    let mut merged: BTreeMap<String, String> = BTreeMap::new();
    let mut elements: Option<Vec<serde_json::Value>> = None;
    let mut current = strip_ns(model).to_string();
    for _ in 0..16 {
        let Some(json) = jar.json(&format!("assets/minecraft/models/{current}.json")) else {
            break;
        };
        if let Some(t) = json.get("textures").and_then(|x| x.as_object()) {
            for (k, v) in t {
                if let Some(s) = v.as_str() {
                    merged.entry(k.clone()).or_insert_with(|| s.to_string());
                }
            }
        }
        if elements.is_none() {
            if let Some(e) = json.get("elements").and_then(|x| x.as_array()) {
                elements = Some(e.clone());
            }
        }
        match json.get("parent").and_then(|x| x.as_str()) {
            Some(p) => current = strip_ns(p).to_string(),
            None => break,
        }
    }
    // 解引用 `#side` 这类变量
    let snapshot = merged.clone();
    for value in merged.values_mut() {
        let mut seen = 0;
        while let Some(var) = value.strip_prefix('#') {
            let Some(next) = snapshot.get(var) else { break };
            *value = next.clone();
            seen += 1;
            if seen > 8 {
                break;
            }
        }
    }
    ResolvedModel { textures: merged, elements }
}

/// 把 `#platform` 这样的引用解成真实贴图路径。
fn deref<'a>(textures: &'a BTreeMap<String, String>, value: &'a str) -> Option<&'a str> {
    let mut cur = value;
    for _ in 0..8 {
        match cur.strip_prefix('#') {
            Some(var) => cur = textures.get(var)?.as_str(),
            None => return Some(cur),
        }
    }
    None
}

/// face 里显式写的 `"uv": [u1,v1,u2,v2]`，单位 1/16。
///
/// 火把、拉杆、按钮这类模型都靠它把贴图上特定的一小块贴到细长的元素上；
/// 按包围盒推会取错区域。
fn face_uv(face: &serde_json::Value) -> Option<[u8; 4]> {
    let a = face.get("uv")?.as_array()?;
    if a.len() < 4 {
        return None;
    }
    let mut out = [0u8; 4];
    for i in 0..4 {
        out[i] = a[i].as_f64()?.clamp(0.0, 16.0).round() as u8;
    }
    Some(out)
}

/// element 的包围盒，夹到 0..16 并保证每轴有正厚度。
fn element_box(e: &serde_json::Value) -> Option<[i8; 6]> {
    let axis = |key: &str, i: usize| {
        e.get(key)
            .and_then(|v| v.as_array())
            .and_then(|a| a.get(i))
            .and_then(serde_json::Value::as_f64)
    };
    let mut b = [0i8; 6];
    for i in 0..3 {
        let (f, t) = (axis("from", i)?, axis("to", i)?);
        // **不往 0..16 里夹**：MC 的模型本来就会伸出格子外，夹了活塞的杆就断了。
        // 只按 i8 的范围兜底，防着畸形模型把后面的算术冲垮
        let lo = f.min(t).round().clamp(-64.0, 64.0);
        let mut hi = f.max(t).round().clamp(-64.0, 64.0);
        // 铁轨、压力板、红石线这类零厚度模型不能退化成看不见
        if hi <= lo {
            hi = lo + 1.0;
        }
        b[i] = lo as i8;
        b[i + 3] = hi as i8;
    }
    Some(b)
}

// ---------------------------------------------------------------- blockstate 规则

/// 一条规则指向的模型与旋转。
struct ModelRef {
    path: String,
    x: i16,
    y: i16,
}

/// variant 的值可能是对象，也可能是带权重的数组（随机模型）。取第一个。
fn parse_model_ref(v: &serde_json::Value) -> Option<ModelRef> {
    match v {
        serde_json::Value::Object(o) => {
            let rot = |k: &str| {
                o.get(k).and_then(serde_json::Value::as_i64).unwrap_or(0).rem_euclid(360) as i16
            };
            Some(ModelRef { path: o.get("model")?.as_str()?.to_owned(), x: rot("x"), y: rot("y") })
        }
        serde_json::Value::Array(a) => a.iter().find_map(parse_model_ref),
        _ => None,
    }
}

/// multipart 的 `when` 归一化成「或组，组内与，值集合内或」。
///
/// 三种写法都要认：
/// - `{"north":"true"}` 单条件
/// - `{"facing":"north|south"}` 值层面的或
/// - `{"OR":[{..},{..}]}` 条件层面的或
pub(crate) fn parse_when(v: Option<&serde_json::Value>) -> Vec<Vec<(String, Vec<String>)>> {
    let Some(obj) = v.and_then(|x| x.as_object()) else { return Vec::new() };

    if let Some(or) = obj.get("OR").and_then(|x| x.as_array()) {
        return or.iter().flat_map(|c| parse_when(Some(c))).collect();
    }
    if let Some(and) = obj.get("AND").and_then(|x| x.as_array()) {
        let mut group = Vec::new();
        for c in and {
            for g in parse_when(Some(c)) {
                group.extend(g);
            }
        }
        return if group.is_empty() { Vec::new() } else { vec![group] };
    }

    let group: Vec<(String, Vec<String>)> = obj
        .iter()
        .filter_map(|(k, val)| {
            let s = match val {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Bool(b) => b.to_string(),
                serde_json::Value::Number(n) => n.to_string(),
                _ => return None,
            };
            Some((k.clone(), s.split('|').map(str::to_owned).collect()))
        })
        .collect();
    if group.is_empty() {
        Vec::new()
    } else {
        vec![group]
    }
}

// ---------------------------------------------------------------- 贴图

/// 读一张贴图并归一化成 `size×size` 的 RGBA。
///
/// 动态贴图（水、火、海晶灯）是竖着排的帧序列，只取第一帧。
fn load_tile(jar: &Jar, texture: &str, size: u32) -> Option<RgbaImage> {
    let path = format!("assets/minecraft/textures/{}.png", strip_ns(texture));
    let bytes = jar.files.get(&path)?;
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    let frame_h = if h > w && h % w == 0 { w } else { h };

    let mut out = RgbaImage::new(size, size);
    for y in 0..size {
        for x in 0..size {
            // 最近邻：像素画放大绝不能插值
            let sx = (x * w / size).min(w - 1);
            let sy = (y * frame_h / size).min(frame_h - 1);
            out.put_pixel(x, y, *img.get_pixel(sx, sy));
        }
    }
    Some(out)
}

fn average_color(tile: &RgbaImage) -> Option<Rgb> {
    let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
    for p in tile.pixels() {
        if p.0[3] < 32 {
            continue;
        }
        r += p.0[0] as u64;
        g += p.0[1] as u64;
        b += p.0[2] as u64;
        n += 1;
    }
    if n == 0 {
        return None;
    }
    Some([(r / n) as u8, (g / n) as u8, (b / n) as u8])
}

/// 图集构建器：贴图去重 + 打包。
struct AtlasBuilder {
    index: HashMap<String, u16>,
    images: Vec<RgbaImage>,
}

impl AtlasBuilder {
    fn new() -> Self {
        Self { index: HashMap::new(), images: Vec::new() }
    }

    /// 登记一张贴图，返回图块索引与平均色。同名同染色只存一份。
    fn intern(&mut self, jar: &Jar, texture: &str, tint: Option<Rgb>) -> (u16, Option<Rgb>) {
        let key = match tint {
            Some(c) => format!("{texture}#{:02x}{:02x}{:02x}", c[0], c[1], c[2]),
            None => texture.to_string(),
        };
        if let Some(&i) = self.index.get(&key) {
            return (i, average_color(&self.images[i as usize]));
        }
        let Some(mut img) = load_tile(jar, texture, TILE) else { return (NO_TILE, None) };
        if let Some(c) = tint {
            for p in img.pixels_mut() {
                let t = apply_tint([p.0[0], p.0[1], p.0[2]], c);
                p.0[0] = t[0];
                p.0[1] = t[1];
                p.0[2] = t[2];
            }
        }
        let Some(avg) = average_color(&img) else { return (NO_TILE, None) };

        // **保留 alpha**。以前在这里把透明像素合成掉，结果红石线底下多出一层
        // 不该有的暗红方底——红石粉的贴图本来就是「一条线 + 四周全透明」，
        // 合成等于给它糊了个背景。透明该由渲染时按画家顺序混合到下面的方块上。

        if self.images.len() >= NO_FACE as usize {
            return (NO_TILE, Some(avg));
        }
        let i = self.images.len() as u16;
        self.images.push(img);
        self.index.insert(key, i);
        (i, Some(avg))
    }

    /// 从一张大贴图里裁出一块登记为图块。方块实体的贴图是摊开的 UV 图，
    /// 只有裁出对应区域才能当方块的面来用。
    fn intern_crop(
        &mut self,
        jar: &Jar,
        texture: &str,
        rect: (u32, u32, u32, u32),
    ) -> (u16, Option<Rgb>) {
        let key = format!("{texture}@{},{},{},{}", rect.0, rect.1, rect.2, rect.3);
        if let Some(&i) = self.index.get(&key) {
            return (i, average_color(&self.images[i as usize]));
        }
        let path = format!("assets/minecraft/textures/{}.png", strip_ns(texture));
        let Some(bytes) = jar.files.get(&path) else { return (NO_TILE, None) };
        let Ok(src) = image::load_from_memory(bytes) else { return (NO_TILE, None) };
        let src = src.to_rgba8();
        let (sw, sh) = src.dimensions();
        if rect.2 == 0 || rect.3 == 0 {
            return (NO_TILE, None);
        }
        let mut img = RgbaImage::new(TILE, TILE);
        for y in 0..TILE {
            for x in 0..TILE {
                // 最近邻把裁剪区拉伸到 16×16
                let sx = (rect.0 + x * rect.2 / TILE).min(sw.saturating_sub(1));
                let sy = (rect.1 + y * rect.3 / TILE).min(sh.saturating_sub(1));
                img.put_pixel(x, y, *src.get_pixel(sx, sy));
            }
        }
        // 实体贴图是摊开的 UV 图，裁出来的区域本身不透明；强制补满 alpha，
        // 免得边角的空白被当成镂空
        let Some(avg) = average_color(&img) else { return (NO_TILE, None) };
        for p in img.pixels_mut() {
            p.0[3] = 255;
        }
        if self.images.len() >= NO_FACE as usize {
            return (NO_TILE, Some(avg));
        }
        let i = self.images.len() as u16;
        self.images.push(img);
        self.index.insert(key, i);
        (i, Some(avg))
    }

    /// 打包成正方形图集，返回 (PNG, 每行图块数, 图块总数)。
    fn finish(&self) -> Result<(Vec<u8>, u32, u32)> {
        let count = self.images.len() as u32;
        let per_row = (count as f64).sqrt().ceil().max(1.0) as u32;
        let dim = (per_row * TILE).max(TILE);
        let mut atlas = RgbaImage::new(dim, dim);
        for (i, img) in self.images.iter().enumerate() {
            let ox = (i as u32 % per_row) * TILE;
            let oy = (i as u32 / per_row) * TILE;
            for y in 0..TILE {
                for x in 0..TILE {
                    atlas.put_pixel(ox + x, oy + y, *img.get_pixel(x, y));
                }
            }
        }
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new_with_quality(
            Cursor::new(&mut png),
            image::codecs::png::CompressionType::Best,
            image::codecs::png::FilterType::Adaptive,
        )
        .write_image(atlas.as_raw(), atlas.width(), atlas.height(), image::ExtendedColorType::Rgba8)
        .context("图集 PNG 编码失败")?;
        Ok((png, per_row, count))
    }
}

// ---------------------------------------------------------------- 主流程

/// 模型库构建器：把模型路径解析成带六面贴图的 element 列表并去重。
struct ModelBuilder {
    index: HashMap<String, u32>,
    models: Vec<ModelDef>,
}

impl ModelBuilder {
    fn new() -> Self {
        Self { index: HashMap::new(), models: Vec::new() }
    }

    fn intern(
        &mut self,
        jar: &Jar,
        atlas: &mut AtlasBuilder,
        path: &str,
        tint: Option<Rgb>,
        samples: &mut FaceSamples,
    ) -> Option<u32> {
        let key = match tint {
            Some(c) => format!("{path}#{:02x}{:02x}{:02x}", c[0], c[1], c[2]),
            None => path.to_string(),
        };
        if let Some(&i) = self.index.get(&key) {
            // 命中缓存时仍要采样颜色，否则该方块可能拿不到平均色
            if let Some(m) = self.models.get(i as usize) {
                for e in &m.e {
                    samples.note_tiles(&e.f, atlas);
                }
            }
            return Some(i);
        }

        let m = resolve_model(jar, path);
        let elements = m.elements.as_deref()?;
        let mut defs: Vec<ElementDef> = Vec::new();
        for e in elements {
            let Some(faces_obj) = e.get("faces").and_then(|f| f.as_object()) else { continue };
            let Some(bbox) = element_box(e) else { continue };

            // 完全重叠的 element 只保留第一个。
            //
            // MC 会把「高光层」叠在本体上靠混合出效果：红石粉是
            // `#line`（线路图案）+ `#overlay`（高光），草方块是本体 + 侧面草色。
            // 我们不做混合，后画的那层会把前一层整个盖住——红石粉就成了一片纯红。
            if defs.iter().any(|d| d.b == bbox) {
                continue;
            }
            // 默认「模型没声明这一面」。声明了但贴图解析不出来才降级成 NO_TILE
            // （那种情况退回平均色是合理的），两者必须分开——见 NO_FACE 的注释。
            let mut f = [NO_FACE; 6];
            // 缺省 uv 按包围盒推，面里显式写了就以显式的为准
            let mut uv = model::default_uv(bbox);
            let mut rot = [0u8; 6];
            for (idx, key) in [
                (model::UP, "up"),
                (model::DOWN, "down"),
                (model::NORTH, "north"),
                (model::SOUTH, "south"),
                (model::EAST, "east"),
                (model::WEST, "west"),
            ] {
                let Some(face) = faces_obj.get(key) else { continue };
                f[idx] = NO_TILE;
                let Some(raw) = face.get("texture").and_then(|x| x.as_str()) else { continue };
                let Some(tex) = deref(&m.textures, raw).map(str::to_owned) else { continue };
                let (tile, avg) = atlas.intern(jar, &tex, tint);
                f[idx] = tile;
                samples.note(idx, avg);
                if let Some(explicit) = face_uv(face) {
                    uv[idx] = explicit;
                }
                rot[idx] = face
                    .get("rotation")
                    .and_then(serde_json::Value::as_i64)
                    .map_or(0, |d| ((d.rem_euclid(360)) / 90) as u8);
            }
            // 一个面都不画的 element 没有意义，别占着 instance 名额
            if f.iter().all(|t| *t == NO_FACE) {
                continue;
            }
            defs.push(ElementDef { b: bbox, f, uv, rot });
        }
        if defs.is_empty() {
            return None;
        }
        let i = self.models.len() as u32;
        self.models.push(ModelDef { e: defs });
        self.index.insert(key, i);
        Some(i)
    }
}

/// 采样各面的平均色，用于切片视图和没有模型时的兜底。
#[derive(Default)]
struct FaceSamples {
    all: Vec<Rgb>,
    top: Option<Rgb>,
    side: Option<Rgb>,
}

impl FaceSamples {
    fn note(&mut self, face: usize, avg: Option<Rgb>) {
        let Some(a) = avg else { return };
        self.all.push(a);
        if face == model::UP {
            self.top.get_or_insert(a);
        } else if face == model::NORTH {
            self.side.get_or_insert(a);
        }
    }

    fn note_tiles(&mut self, faces: &[u16; 6], atlas: &AtlasBuilder) {
        for (idx, t) in faces.iter().enumerate() {
            if *t == NO_TILE || *t == NO_FACE {
                continue;
            }
            let avg = atlas.images.get(*t as usize).and_then(average_color);
            self.note(idx, avg);
        }
    }

    fn mean(&self) -> Option<Rgb> {
        if self.all.is_empty() {
            return None;
        }
        let n = self.all.len() as u32;
        Some([
            (self.all.iter().map(|c| c[0] as u32).sum::<u32>() / n) as u8,
            (self.all.iter().map(|c| c[1] as u32).sum::<u32>() / n) as u8,
            (self.all.iter().map(|c| c[2] as u32).sum::<u32>() / n) as u8,
        ])
    }
}

/// 方块实体（箱子、末影箱、潜影盒…）在游戏里由专门的渲染器绘制，
/// blockstate 指向 `builtin/entity`，模型里只有 `particle`。
///
/// 拿 particle 去贴满立方体会明显错——箱子的 particle 是橡木板，
/// 那样箱子就成了一整块木板。它们真正的贴图在 `textures/entity/` 下，
/// 是按实体模型摊开的 UV 图，这里按已知布局裁出「顶面」和「正面」两块。
///
/// 返回 (贴图路径, 顶面裁剪区, 侧面裁剪区)，裁剪区为 (x, y, w, h)。
fn entity_faces(block: &str) -> Option<(String, (u32, u32, u32, u32), (u32, u32, u32, u32))> {
    let id = block.strip_prefix("minecraft:")?;
    // 单体箱子的贴图是 64×64：箱盖顶面在 (14,0) 14×14，
    // 箱盖正面在 (14,14) 14×5，箱体正面在 (14,33) 14×10。
    // 侧面取箱体正面那块（带锁扣，最有辨识度）。
    let chest = |name: &str| {
        Some((
            format!("entity/chest/{name}"),
            (14u32, 0u32, 14u32, 14u32),
            (14u32, 33u32, 14u32, 10u32),
        ))
    };
    match id {
        "chest" => chest("normal"),
        "trapped_chest" => chest("trapped"),
        "ender_chest" => chest("ender"),
        _ => None,
    }
}

/// 中文名。lang 的键形如 `block.minecraft.stone` / `item.minecraft.redstone`。
///
/// **jar 里只有 `en_us.json`**——其它语言由启动器单独下载，存在
/// `.minecraft/assets/objects/<hash 前两位>/<hash>`，由
/// `.minecraft/assets/indexes/*.json` 索引。所以除了 jar 内，
/// 还要顺着 jar 路径往上找到 `.minecraft` 去资源仓库里捞。
fn chinese_names(jar: &Jar, jar_path: &Path) -> BTreeMap<String, String> {
    let raw = jar
        .files
        .get("assets/minecraft/lang/zh_cn.json")
        .cloned()
        .or_else(|| zh_cn_from_asset_store(jar_path));

    let mut names = BTreeMap::new();
    let Some(raw) = raw else { return names };
    let Ok(serde_json::Value::Object(obj)) = serde_json::from_slice(&raw) else { return names };
    for (k, v) in &obj {
        let Some(text) = v.as_str() else { continue };
        // 方块名优先：同一个 ID 既是方块又是物品时，方块译名更贴合展示
        if let Some(id) = k.strip_prefix("block.minecraft.") {
            names.insert(format!("minecraft:{id}"), text.to_owned());
        } else if let Some(id) = k.strip_prefix("item.minecraft.") {
            names.entry(format!("minecraft:{id}")).or_insert_with(|| text.to_owned());
        }
    }
    names
}

/// 从启动器的资源仓库里捞 `zh_cn.json`。
///
/// jar 通常在 `.minecraft/versions/<版本>/<版本>.jar`，但**不能写死「往上两级」**：
/// 这台机器上的 1.21.4 客户端就放在 `.minecraft/1.21.4/1.21.4.jar`，
/// 只往上两级会找到 `.minecraft` 的上一层，一条中文名都捞不到，
/// 而且是静默降级成英文 ID，很难发现。改成逐级往上找到有 `assets/indexes` 的那层。
///
/// 索引文件可能有好几份（对应不同版本），全扫一遍取能读到且最大的那份——
/// 新版本的翻译条目更全。
fn zh_cn_from_asset_store(jar_path: &Path) -> Option<Vec<u8>> {
    let mc_root = jar_path
        .ancestors()
        .skip(1)
        .find(|d| d.join("assets").join("indexes").is_dir())?;
    let indexes = mc_root.join("assets").join("indexes");
    let objects = mc_root.join("assets").join("objects");

    let mut best: Option<Vec<u8>> = None;
    for entry in std::fs::read_dir(&indexes).ok()?.flatten() {
        let Ok(text) = std::fs::read_to_string(entry.path()) else { continue };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        let Some(hash) = json
            .get("objects")
            .and_then(|o| o.get("minecraft/lang/zh_cn.json"))
            .and_then(|o| o.get("hash"))
            .and_then(|h| h.as_str())
        else {
            continue;
        };
        if hash.len() < 2 {
            continue;
        }
        let path = objects.join(&hash[..2]).join(hash);
        let Ok(bytes) = std::fs::read(&path) else { continue };
        if best.as_ref().map_or(true, |b| bytes.len() > b.len()) {
            best = Some(bytes);
        }
    }
    best
}

/// 从 jar 提取全部方块模型、材质与中文名。
pub fn extract(jar_path: &Path) -> Result<Extracted> {
    let jar = Jar::open(jar_path)?;

    let version = jar
        .json("version.json")
        .and_then(|v| v.get("name").and_then(|x| x.as_str()).map(str::to_owned))
        .unwrap_or_else(|| "unknown".into());

    let mut block_names: Vec<String> = jar
        .files
        .keys()
        .filter_map(|k| {
            k.strip_prefix("assets/minecraft/blockstates/")?
                .strip_suffix(".json")
                .map(str::to_owned)
        })
        .collect();
    block_names.sort();

    let mut atlas = AtlasBuilder::new();
    let mut models = ModelBuilder::new();
    let mut colors: BTreeMap<String, BlockColor> = BTreeMap::new();
    let mut blocks: BTreeMap<String, BlockDef> = BTreeMap::new();
    let mut unresolved: Vec<String> = Vec::new();

    for name in block_names {
        let block_id = format!("minecraft:{name}");
        let Some(bs) = jar.json(&format!("assets/minecraft/blockstates/{name}.json")) else {
            unresolved.push(block_id);
            continue;
        };
        let tint = biome_tint(&block_id);
        let mut samples = FaceSamples::default();
        let mut def: Option<BlockDef> = None;

        if let Some(variants) = bs.get("variants").and_then(|x| x.as_object()) {
            // 键排序保证输出可复现
            let mut keys: Vec<&String> = variants.keys().collect();
            keys.sort();
            let mut list = Vec::new();
            for k in keys {
                let Some(r) = parse_model_ref(&variants[k]) else { continue };
                let Some(m) =
                    models.intern(&jar, &mut atlas, strip_ns(&r.path), tint, &mut samples)
                else {
                    continue;
                };
                list.push(VariantDef { w: k.clone(), m, x: r.x, y: r.y });
            }
            if !list.is_empty() {
                def = Some(BlockDef::Variants(list));
            }
        } else if let Some(parts) = bs.get("multipart").and_then(|x| x.as_array()) {
            let mut list = Vec::new();
            for p in parts {
                let Some(r) = p.get("apply").and_then(parse_model_ref) else { continue };
                let Some(m) =
                    models.intern(&jar, &mut atlas, strip_ns(&r.path), tint, &mut samples)
                else {
                    continue;
                };
                list.push(PartDef { w: parse_when(p.get("when")), m, x: r.x, y: r.y });
            }
            if !list.is_empty() {
                def = Some(BlockDef::Multipart(list));
            }
        }

        // 箱子这类方块实体没有模型，但有实体贴图，裁出来就能当普通方块画
        if def.is_none() {
            if let Some((tex, top_rect, side_rect)) = entity_faces(&block_id) {
                let (top_tile, top_avg) = atlas.intern_crop(&jar, &tex, top_rect);
                let (side_tile, side_avg) = atlas.intern_crop(&jar, &tex, side_rect);
                if top_tile != NO_TILE || side_tile != NO_TILE {
                    let t = if top_tile == NO_TILE { side_tile } else { top_tile };
                    let sd = if side_tile == NO_TILE { top_tile } else { side_tile };
                    // 箱子实际是 14×14×14 居中，比整格略小
                    let b = [1i8, 0, 1, 15, 14, 15];
                    let e = ElementDef {
                        b,
                        f: [t, sd, sd, sd, sd, sd],
                        uv: model::default_uv(b),
                        rot: [0; 6],
                    };
                    let m = models.models.len() as u32;
                    models.models.push(ModelDef { e: vec![e] });
                    def = Some(BlockDef::Variants(vec![VariantDef {
                        w: String::new(),
                        m,
                        x: 0,
                        y: 0,
                    }]));
                    samples.note(model::UP, top_avg);
                    samples.note(model::NORTH, side_avg);
                }
            }
        }

        // 其余没有模型的方块实体（告示牌、床、潜影盒…）只能取 particle 的
        // 平均色兜底。**绝不能**拿 particle 去贴满一个立方体——
        // 箱子的 particle 是橡木板，那样箱子会变成一整块木板。
        let mut mean = samples.mean();
        if mean.is_none() {
            let particle = bs
                .get("variants")
                .and_then(|x| x.as_object())
                .and_then(|o| o.values().next())
                .and_then(parse_model_ref)
                .map(|r| resolve_model(&jar, strip_ns(&r.path)))
                .and_then(|m| {
                    m.textures
                        .get("particle")
                        .cloned()
                        .or_else(|| m.textures.values().find(|v| !v.starts_with('#')).cloned())
                });
            if let Some(tex) = particle {
                mean = atlas.intern(&jar, &tex, tint).1;
            }
        }

        match mean {
            Some(a) => {
                colors.insert(
                    block_id.clone(),
                    BlockColor { top: samples.top.unwrap_or(a), side: samples.side.unwrap_or(a) },
                );
            }
            None => {
                unresolved.push(block_id);
                continue;
            }
        }
        if let Some(d) = def {
            blocks.insert(block_id, d);
        }
    }

    let (atlas_png, per_row, count) = atlas.finish()?;
    unresolved.sort();
    unresolved.dedup();

    Ok(Extracted {
        assets: BlockAssets {
            version,
            colors,
            models: models.models,
            blocks,
            names: chinese_names(&jar, jar_path),
            tile_size: TILE,
            tiles_per_row: per_row,
            tile_count: count,
            unresolved,
        },
        atlas_png,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespace_stripping() {
        assert_eq!(strip_ns("minecraft:block/stone"), "block/stone");
        assert_eq!(strip_ns("block/stone"), "block/stone");
    }

    #[test]
    fn model_ref_reads_rotation() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"model":"minecraft:block/piston","x":270,"y":90}"#).unwrap();
        let r = parse_model_ref(&v).unwrap();
        assert_eq!(r.path, "minecraft:block/piston");
        assert_eq!((r.x, r.y), (270, 90));
    }

    #[test]
    fn model_ref_from_weighted_array_takes_first() {
        let v: serde_json::Value = serde_json::from_str(
            r#"[{"model":"minecraft:block/a","weight":3},{"model":"minecraft:block/b"}]"#,
        )
        .unwrap();
        assert_eq!(parse_model_ref(&v).unwrap().path, "minecraft:block/a");
    }

    #[test]
    fn when_parses_simple_condition() {
        let v: serde_json::Value = serde_json::from_str(r#"{"north":"true"}"#).unwrap();
        let w = parse_when(Some(&v));
        assert_eq!(w, vec![vec![("north".to_string(), vec!["true".to_string()])]]);
    }

    #[test]
    fn when_parses_value_level_or() {
        let v: serde_json::Value = serde_json::from_str(r#"{"facing":"north|south"}"#).unwrap();
        let w = parse_when(Some(&v));
        assert_eq!(
            w,
            vec![vec![(
                "facing".to_string(),
                vec!["north".to_string(), "south".to_string()]
            )]]
        );
    }

    #[test]
    fn when_parses_or_block() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"OR":[{"north":"true"},{"south":"true"}]}"#).unwrap();
        let w = parse_when(Some(&v));
        assert_eq!(w.len(), 2, "OR 应展开成两个并列组");
    }

    #[test]
    fn when_absent_means_always() {
        assert!(parse_when(None).is_empty());
    }

    #[test]
    fn element_box_gives_thickness_to_flat_models() {
        let e: serde_json::Value =
            serde_json::from_str(r#"{"from":[0,0.25,0],"to":[16,0.25,16]}"#).unwrap();
        let b = element_box(&e).unwrap();
        assert!(b[4] > b[1], "零厚度的铁轨也要有正厚度: {b:?}");
    }

    /// 关键回归：伸出格子外的 element **不能**夹回 0..16。
    ///
    /// 活塞头的杆是 `from[6,6,4] to[10,10,20]`，最后 4 格故意插进活塞本体那一格的
    /// 凹槽里。以前夹到 16 就把杆截断在格子边界上，活塞看起来「杆和本体是分开的」。
    #[test]
    fn element_box_keeps_geometry_outside_the_cell() {
        let arm: serde_json::Value =
            serde_json::from_str(r#"{"from":[6,6,4],"to":[10,10,20]}"#).unwrap();
        assert_eq!(element_box(&arm).unwrap(), [6, 6, 4, 10, 10, 20], "活塞杆被截断了");

        // 墙上火把扎进墙里半格
        let wall_torch: serde_json::Value =
            serde_json::from_str(r#"{"from":[-1,3.5,7],"to":[1,13.5,9]}"#).unwrap();
        let b = element_box(&wall_torch).unwrap();
        assert_eq!([b[0], b[3]], [-1, 1], "墙上火把该保持 2 宽并跨过格子边界");
    }

    #[test]
    fn average_color_ignores_transparent_pixels() {
        let mut img = RgbaImage::new(2, 1);
        img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        img.put_pixel(1, 0, image::Rgba([0, 255, 0, 0]));
        assert_eq!(average_color(&img), Some([255, 0, 0]));
    }

    #[test]
    fn tint_applied_to_grayscale_blocks() {
        assert!(biome_tint("minecraft:grass_block").is_some());
        assert!(biome_tint("minecraft:oak_leaves").is_some());
        assert!(biome_tint("minecraft:cherry_leaves").is_none());
        assert!(biome_tint("minecraft:stone").is_none());
        assert_eq!(biome_tint("minecraft:spruce_leaves"), Some([0x61, 0x99, 0x61]));
    }

    #[test]
    fn tint_multiplies() {
        assert_eq!(apply_tint([255, 255, 255], [0x91, 0xBD, 0x59]), [0x91, 0xBD, 0x59]);
        assert_eq!(apply_tint([0, 0, 0], [0x91, 0xBD, 0x59]), [0, 0, 0]);
    }
}
