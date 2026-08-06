//! 蓝图渲染：等距缩略图、Y 轴切片、以及供前端 WebGL 用的体素模型。
//!
//! 三种视图各有分工——生电蓝图从外面看基本都是个方盒子，等距图只给个轮廓印象，
//! 内部电路要靠逐层切片，想转着看则要交给前端做真正的 3D。
//!
//! 体积可达 5.15 亿的蓝图不可能逐方块画，先降采样到有界的体素网格再处理。
//! 每格记录**主导方块**（Boyer–Moore 多数投票）而不是平均颜色：
//! 平均色会把降采样区域糊成灰调，而且悬停时报不出方块名。

use std::collections::HashMap;

use anyhow::Result;
use image::{Rgba, RgbaImage};
use serde::Serialize;

use litematic::schematic::Vec3i;
use litematic::Schematic;
use mcassets::{BlockAssets, BlockColor, ResolvedBox, Rgb, NO_FACE, NO_TILE};

/// 贴图的透明度类别。**决定这一格挡不挡得住邻居**，也决定前端该拿哪种材质画。
///
/// 只看几何（`is_full_cube`）是不够的：玻璃、树叶都是严丝合缝的完整立方体，
/// 但看得穿。按几何剔除会把玻璃罩里的整台机器都剔掉。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Opacity {
    /// 每个纹素都不透明，挡得住后面的东西
    Opaque,
    /// 有全透明的纹素，其余不透明——树叶、铁栏杆、原版玻璃。
    /// 前端用 alphaTest 剪切即可，不需要排序
    Cutout,
    /// 存在半透明纹素——染色玻璃、淡化玻璃。
    /// 必须真的做 alpha 混合，alphaTest 对它完全无效
    Translucent,
}

/// 低于此值当作全透，高于 `SOLID` 当作全不透，中间是半透。
const CLEAR: u8 = 16;
const SOLID: u8 = 250;

/// 一张图块的透明度。个别游离的透明像素不算数（要求至少 1/64 的面积），
/// 否则贴图边缘的一两个杂点就会让整个方块不再遮挡，白白多出一堆体素。
fn tile_opacity(image: &RgbaImage, ox: u32, oy: u32, size: u32) -> Opacity {
    let (mut clear, mut partial, mut n) = (0u32, 0u32, 0u32);
    for y in oy..(oy + size).min(image.height()) {
        for x in ox..(ox + size).min(image.width()) {
            let a = image.get_pixel(x, y).0[3];
            n += 1;
            if a < CLEAR {
                clear += 1;
            } else if a < SOLID {
                partial += 1;
            }
        }
    }
    if partial > 0 {
        Opacity::Translucent
    } else if n > 0 && clear * 64 >= n {
        Opacity::Cutout
    } else {
        Opacity::Opaque
    }
}

/// 方块图集。等距渲染与切片都从这里取纹素。
pub struct Atlas {
    pub image: RgbaImage,
    pub tile_size: u32,
    pub tiles_per_row: u32,
    /// 每个图块的透明度类别，建图集时一次扫出来。
    /// 一千来个图块 × 256 像素，一次几毫秒，比每次查询现算划算得多。
    opacity: Vec<Opacity>,
}

impl Atlas {
    pub fn new(image: RgbaImage, tile_size: u32, tiles_per_row: u32) -> Self {
        let ts = tile_size.max(1);
        let per_row = tiles_per_row.max(1);
        let rows = image.height() / ts;
        let mut opacity = Vec::with_capacity((rows * per_row) as usize);
        for row in 0..rows {
            for col in 0..per_row {
                opacity.push(tile_opacity(&image, col * ts, row * ts, ts));
            }
        }
        Self { image, tile_size, tiles_per_row, opacity }
    }

    /// 图块的透明度。
    ///
    /// - `NO_TILE`：这面要画但没贴图，退回平均色填充——画出来是实心的，算不透明。
    /// - `NO_FACE`：这面根本不画，那就是个洞，**看得穿**，不能当遮挡物。
    pub fn tile_class(&self, tile: u16) -> Opacity {
        if tile == NO_FACE {
            return Opacity::Cutout;
        }
        if tile == NO_TILE {
            return Opacity::Opaque;
        }
        self.opacity.get(tile as usize).copied().unwrap_or(Opacity::Opaque)
    }

    /// 一个方块（可能由多个长方体组成）整体的透明度：取所有面里最「透」的那一档。
    pub fn block_class(&self, boxes: &[ResolvedBox]) -> Opacity {
        let mut worst = Opacity::Opaque;
        for b in boxes {
            for &f in &b.faces {
                match self.tile_class(f) {
                    Opacity::Translucent => return Opacity::Translucent,
                    Opacity::Cutout => worst = Opacity::Cutout,
                    Opacity::Opaque => {}
                }
            }
        }
        worst
    }

    pub fn from_png(bytes: &[u8], tile_size: u32, tiles_per_row: u32) -> Result<Self> {
        let image = image::load_from_memory(bytes)?.to_rgba8();
        Ok(Self::new(image, tile_size, tiles_per_row))
    }

    /// 取 `tile` 号图块内 (tx, ty) 处的纹素。越界返回 None。
    #[inline]
    fn texel(&self, tile: u16, tx: u32, ty: u32) -> Option<[u8; 4]> {
        if tile == NO_TILE || tile == NO_FACE || self.tiles_per_row == 0 {
            return None;
        }
        let t = tile as u32;
        let ox = (t % self.tiles_per_row) * self.tile_size;
        let oy = (t / self.tiles_per_row) * self.tile_size;
        let x = ox + tx.min(self.tile_size - 1);
        let y = oy + ty.min(self.tile_size - 1);
        if x >= self.image.width() || y >= self.image.height() {
            return None;
        }
        Some(self.image.get_pixel(x, y).0)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RenderOptions {
    /// 降采样后网格的最大边长。越大越精细，内存与耗时也越高。
    pub target_grid: u32,
    /// 输出图片的最大边长
    pub max_px: u32,
    /// 背景色；None 表示透明
    pub background: Option<Rgb>,
    /// 找不到颜色的方块用什么色
    pub fallback: Rgb,
    /// 降采样格子被判定为实心所需的占据率（0.0~1.0）。
    ///
    /// 默认 0，即「格子里有任何非空气方块就算实心」。
    /// 别想当然地调高：像素画和幕墙是薄片结构，一个 12³ 的格子里最多
    /// 只有 144 个方块（占据率 8.3%），阈值一旦超过它整张图就全空了。
    pub density_threshold: f32,
    /// 超采样倍率。以 N 倍分辨率渲染再缩回来，边缘就不会有锯齿。
    pub supersample: u32,
    /// 方块描边强度（0~100，0 为不描边）。
    pub edge_strength: u32,
    /// 一个方块在屏幕上至少占多少像素才值得贴材质。
    /// 低于这个尺寸纹理会被压成噪点，还不如用平均色干净。
    pub min_texture_px: i32,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            target_grid: 128,
            max_px: 512,
            background: None,
            fallback: [0x9E, 0x9E, 0x9E],
            density_threshold: 0.0,
            supersample: 2,
            edge_strength: 22,
            min_texture_px: 5,
        }
    }
}

/// 体素网格里的一种方块（对应蓝图调色板里的一个方块状态）。
#[derive(Debug, Clone, Serialize)]
pub struct VoxelBlock {
    /// 完整命名空间 ID，例如 `minecraft:sticky_piston`
    pub name: String,
    pub top: Rgb,
    pub side: Rgb,
    /// 按真实方块状态解析出的长方体。楼梯有两段、栅栏有柱子和横杆，
    /// 活塞/活板门/梯子的朝向也在这里体现。空表示没有模型（方块实体），
    /// 退回用平均色画一个完整立方体。
    pub boxes: Vec<ResolvedBox>,
    /// 贴图透明度。没有图集时一律 `Opaque`（那种情况本来就按平均色画实心）。
    pub opacity: Opacity,
    /// 降采样时的代表图块 `(顶面, 侧面)`。
    ///
    /// 一个体素代表 scale³ 个方块时按完整立方体画，六面就用这两张图。
    /// **以前这里直接退回平均色**，于是只要蓝图最长边超过 `target_grid`
    /// 就整张缩略图都是色块——用户看到的「有些蓝图有材质、有些是色块」就是这个，
    /// 而且分界线是蓝图尺寸，看起来毫无规律。
    pub repr: (u16, u16),
}

impl VoxelBlock {
    /// 是否严丝合缝地填满一格。**填满还不够，还得不透明才挡得住邻居**——见 `blocks_sight`。
    pub fn is_full_cube(&self) -> bool {
        self.boxes.is_empty() || (self.boxes.len() == 1 && self.boxes[0].is_full_cube())
    }
    /// 能不能挡住后面的东西。玻璃、树叶是完整立方体但看得穿，不算。
    pub fn blocks_sight(&self) -> bool {
        self.is_full_cube() && self.opacity == Opacity::Opaque
    }
    /// 降采样时用的整格长方体：六面套代表图块。
    pub fn repr_box(&self) -> ResolvedBox {
        const FULL: [i8; 6] = [0, 0, 0, 16, 16, 16];
        let (top, side) = self.repr;
        ResolvedBox {
            bbox: FULL,
            // 面序：up, down, north, south, east, west
            faces: [top, top, side, side, side, side],
            uv: mcassets::model::default_uv(FULL),
            rot: [0; 6],
        }
    }
    /// 没有模型时按完整立方体的平均色画
    pub fn is_flat_color(&self) -> bool {
        self.boxes.is_empty()
    }
}

/// 挑出一个方块的代表图块 `(顶面, 侧面)`，降采样成一个体素时用它贴满整格。
///
/// 顶面取**真的画顶面**的长方体里最高的那个；侧面取体积最大的那个长方体上
/// 第一个画出来的侧面。都挑不到就退回 `NO_TILE`（平均色）。
fn repr_tiles(boxes: &[ResolvedBox]) -> (u16, u16) {
    use mcassets::model::{face_drawn, EAST, NORTH, SOUTH, UP, WEST};
    let top = boxes
        .iter()
        .filter(|b| face_drawn(b.faces[UP]))
        .max_by_key(|b| b.bbox[4])
        .map_or(NO_TILE, |b| b.faces[UP]);
    let volume = |b: &ResolvedBox| {
        (b.bbox[3] as i32 - b.bbox[0] as i32)
            * (b.bbox[4] as i32 - b.bbox[1] as i32)
            * (b.bbox[5] as i32 - b.bbox[2] as i32)
    };
    let side = boxes
        .iter()
        .max_by_key(|b| volume(b))
        .and_then(|b| [SOUTH, EAST, NORTH, WEST].iter().map(|&f| b.faces[f]).find(|t| face_drawn(*t)))
        .unwrap_or(NO_TILE);
    (top, if face_drawn(side) { side } else { top })
}

/// 调色板去重的键：方块名 + 属性。
///
/// 只按名字去重会把朝东和朝上的活塞、上半和下半的活板门合并成同一项，
/// 而它们的几何和贴图完全不同。
fn state_key(bs: &litematic::BlockState) -> String {
    if bs.properties.is_empty() {
        return bs.name.clone();
    }
    let mut s = String::with_capacity(bs.name.len() + 32);
    s.push_str(&bs.name);
    for (k, v) in &bs.properties {
        s.push(';');
        s.push_str(k);
        s.push('=');
        s.push_str(v);
    }
    s
}

/// 单个格子。刻意不存累加颜色——那样每格要 28 字节，
/// 192³ 的网格就是 190 MB；存主导方块索引只要 12 字节。
#[derive(Clone, Copy)]
struct Cell {
    count: u32,
    /// Boyer–Moore 多数投票的候选者（全局调色板索引）
    cand: u16,
    votes: u32,
}

impl Default for Cell {
    fn default() -> Self {
        Self { count: 0, cand: u16::MAX, votes: 0 }
    }
}

impl Cell {
    /// Boyer–Moore 多数投票：O(1) 内存求主导元素。
    /// 没有绝对多数时结果不保证是众数，但预览够用；
    /// 而且大多数蓝图 scale=1，一格就是一个方块，本就精确。
    #[inline]
    fn vote(&mut self, idx: u16) {
        self.count += 1;
        if self.votes == 0 {
            self.cand = idx;
            self.votes = 1;
        } else if self.cand == idx {
            self.votes += 1;
        } else {
            self.votes -= 1;
        }
    }
}

/// 降采样后的体素网格。
pub struct VoxelGrid {
    pub w: usize,
    pub h: usize,
    pub d: usize,
    /// 每个格子代表原图多少个方块（每轴）
    pub scale: u32,
    /// 原蓝图包围盒的最小角
    pub origin: Vec3i,
    /// 全局调色板：所有 region 的方块合并去重后的表
    pub palette: Vec<VoxelBlock>,
    cells: Vec<Cell>,
    fill_threshold: u32,
}

impl VoxelGrid {
    #[inline]
    fn idx(&self, x: usize, y: usize, z: usize) -> usize {
        (y * self.d + z) * self.w + x
    }

    #[inline]
    pub fn is_solid(&self, x: usize, y: usize, z: usize) -> bool {
        self.cells[self.idx(x, y, z)].count >= self.fill_threshold
    }

    #[inline]
    pub fn block_index_at(&self, x: usize, y: usize, z: usize) -> Option<u16> {
        let c = &self.cells[self.idx(x, y, z)];
        if c.count >= self.fill_threshold && (c.cand as usize) < self.palette.len() {
            Some(c.cand)
        } else {
            None
        }
    }

    pub fn block_at(&self, x: usize, y: usize, z: usize) -> Option<&VoxelBlock> {
        self.palette.get(self.block_index_at(x, y, z)? as usize)
    }

    pub fn solid_count(&self) -> usize {
        self.cells.iter().filter(|c| c.count >= self.fill_threshold).count()
    }

    /// 六邻接里至少有一面裸露的格子。
    ///
    /// 内部格子在任何视角都看不见，剔掉能把实心建筑的体素数从 O(n³) 降到 O(n²)
    /// ——这是能把整个模型送去前端做 WebGL 的前提。
    /// 这一格能否挡住邻居。只有**填满整格且不透明**才挡得住——
    /// 火把、活板门、栅栏之间是看得见缝的，玻璃和树叶则是填满了但看得穿，
    /// 按实心处理都会把邻居错误剔掉（玻璃罩里的整台机器会整个消失）。
    #[inline]
    fn occludes(&self, x: usize, y: usize, z: usize) -> bool {
        self.block_index_at(x, y, z)
            .and_then(|bi| self.palette.get(bi as usize))
            // 降采样时一格代表多个方块，前后端都统一按平均色的实心立方体画。
            // 既然画出来是实心的，就该按实心剔除，否则白送一堆看不见的内部体素。
            .is_some_and(|b| if self.scale > 1 { b.is_full_cube() } else { b.blocks_sight() })
    }

    pub fn surface_voxels(&self) -> Vec<(u16, u16, u16, u16)> {
        let mut out = Vec::new();
        for y in 0..self.h {
            for z in 0..self.d {
                for x in 0..self.w {
                    let Some(bi) = self.block_index_at(x, y, z) else { continue };
                    // 非完整方块自己就露在外面，任何情况下都要画
                    let partial = self
                        .palette
                        .get(bi as usize)
                        .is_some_and(|b| !b.is_full_cube());
                    let exposed = partial
                        || x == 0
                        || y == 0
                        || z == 0
                        || x + 1 == self.w
                        || y + 1 == self.h
                        || z + 1 == self.d
                        || !self.occludes(x - 1, y, z)
                        || !self.occludes(x + 1, y, z)
                        || !self.occludes(x, y - 1, z)
                        || !self.occludes(x, y + 1, z)
                        || !self.occludes(x, y, z - 1)
                        || !self.occludes(x, y, z + 1);
                    if exposed {
                        out.push((x as u16, y as u16, z as u16, bi));
                    }
                }
            }
        }
        out
    }

    /// 把蓝图降采样成体素网格。
    ///
    /// `atlas` 只用来判定每种方块透不透（决定遮挡剔除与前端材质）。传 `None`
    /// 时一律按不透明处理——那种情况本来就是退回平均色画实心立方体。
    /// **参数刻意不是可选的**：漏传就等于玻璃又变回不透明，让编译器盯着。
    pub fn build(
        schem: &Schematic,
        assets: &BlockAssets,
        atlas: Option<&Atlas>,
        opts: &RenderOptions,
    ) -> Option<Self> {
        let (lo, hi) = schem.bounding_box()?;
        let dims = (
            (hi.x - lo.x + 1).max(1) as u32,
            (hi.y - lo.y + 1).max(1) as u32,
            (hi.z - lo.z + 1).max(1) as u32,
        );
        let longest = dims.0.max(dims.1).max(dims.2);
        let target = opts.target_grid.max(1);
        let scale = longest.div_ceil(target).max(1);

        let w = dims.0.div_ceil(scale) as usize;
        let h = dims.1.div_ceil(scale) as usize;
        let d = dims.2.div_ceil(scale) as usize;

        let cell_volume = (scale as u64).pow(3);
        let fill_threshold = if opts.density_threshold <= 0.0 {
            1
        } else {
            ((cell_volume as f64 * opts.density_threshold as f64).round() as u64).max(1) as u32
        };

        let mut palette: Vec<VoxelBlock> = Vec::new();
        let mut lookup: HashMap<String, u16> = HashMap::new();
        let mut cells = vec![Cell::default(); w * h * d];

        for region in &schem.regions {
            // 先把 region 调色板映射到全局调色板，循环体里就只剩整数运算
            let mapped: Vec<Option<u16>> = region
                .palette
                .iter()
                .map(|bs| {
                    if bs.is_air() {
                        return None;
                    }
                    // 键必须带上属性：朝东和朝上的活塞是两个不同的模型，
                    // 只按名字去重会让它们共用同一套几何
                    let key = state_key(bs);
                    if let Some(&i) = lookup.get(&key) {
                        return Some(i);
                    }
                    if palette.len() >= u16::MAX as usize {
                        return None;
                    }
                    let c = assets.get(&bs.name).unwrap_or(BlockColor {
                        top: opts.fallback,
                        side: opts.fallback,
                    });
                    let i = palette.len() as u16;
                    // 用方块的真实状态去解析模型
                    let boxes = assets.resolve(&bs.name, &bs.properties);
                    let opacity =
                        atlas.map_or(Opacity::Opaque, |at| at.block_class(&boxes));
                    let repr = repr_tiles(&boxes);
                    palette.push(VoxelBlock {
                        name: bs.name.clone(),
                        top: c.top,
                        side: c.side,
                        boxes,
                        opacity,
                        repr,
                    });
                    lookup.insert(key, i);
                    Some(i)
                })
                .collect();

            let min = region.min_corner();
            let ext = region.extent();
            let (sx, sz) = (ext.x.max(1) as u64, ext.z.max(1) as u64);
            let plane = sx * sz;

            region.for_each_block(|index, pi| {
                let Some(Some(bi)) = mapped.get(pi as usize) else { return };
                // 内联 index → 局部坐标，避免每个方块一次函数调用
                let ly = index / plane;
                let rem = index % plane;
                let lz = rem / sx;
                let lx = rem % sx;

                let gx = (min.x as i64 + lx as i64 - lo.x as i64) as u64 / scale as u64;
                let gy = (min.y as i64 + ly as i64 - lo.y as i64) as u64 / scale as u64;
                let gz = (min.z as i64 + lz as i64 - lo.z as i64) as u64 / scale as u64;

                if gx as usize >= w || gy as usize >= h || gz as usize >= d {
                    return;
                }
                let i = (gy as usize * d + gz as usize) * w + gx as usize;
                cells[i].vote(*bi);
            });
        }

        Some(Self { w, h, d, scale, origin: lo, palette, cells, fill_threshold })
    }

    /// 诊断用：把给定的方块各摆一个孤立立方体，排成一行。
    ///
    /// 用来核对「方块名 → 图块 → 图集 → 渲染」整条链路对不对得上。
    /// 光看渲染出来的机器猜不出哪块是什么，必须有已知答案的对照。
    pub fn sample_row(
        names: &[String],
        props: &[std::collections::BTreeMap<String, String>],
        assets: &BlockAssets,
        atlas: Option<&Atlas>,
        opts: &RenderOptions,
    ) -> Self {
        let empty = std::collections::BTreeMap::new();
        let palette: Vec<VoxelBlock> = names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let c = assets
                    .get(name)
                    .unwrap_or(BlockColor { top: opts.fallback, side: opts.fallback });
                let boxes = assets.resolve(name, props.get(i).unwrap_or(&empty));
                let opacity = atlas.map_or(Opacity::Opaque, |at| at.block_class(&boxes));
                let repr = repr_tiles(&boxes);
                VoxelBlock { name: name.clone(), top: c.top, side: c.side, boxes, opacity, repr }
            })
            .collect();

        // 沿 x 排开，中间隔一格，免得相邻立方体的面粘在一起分不清
        let n = names.len();
        let w = n * 2;
        let mut cells = vec![Cell::default(); w.max(1)];
        for i in 0..n {
            cells[i * 2].vote(i as u16);
        }
        Self {
            w: w.max(1),
            h: 1,
            d: 1,
            scale: 1,
            origin: Vec3i::default(),
            palette,
            cells,
            fill_threshold: 1,
        }
    }
}

// ---------------------------------------------------------------- 等距投影

/// 2:1 等距投影里，半宽为 u 的方块画在 2u×2u 的精灵框内。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Face {
    None,
    /// 顶面
    Top,
    /// 左半边，实际是 +Z（南）面
    Left,
    /// 右半边，实际是 +X（东）面
    Right,
}

/// 方块局部坐标 (X,Y,Z)∈[0,1]³ 投影到精灵框内的像素坐标。
///
/// 精灵框以「完整方块」为准，尺寸 2u×2u；小于完整方块的形状落在框内的一部分。
#[inline]
fn project(x: f32, y: f32, z: f32, u: f32) -> (f32, f32) {
    ((x - z) * u + u, (x + z) * u / 2.0 - y * u + u)
}

/// 一个可见面：由原点和两条边向量张成的平行四边形，附带逆变换。
struct FacePatch {
    face: Face,
    /// 屏幕原点（参数 (0,0) 处）
    o: (f32, f32),
    /// 逆矩阵，把 (da, db) 换算成参数 (p, q)
    inv: [f32; 4],
}

impl FacePatch {
    /// `o` 是参数原点的三维位置，`ep`/`eq` 是两条边的三维增量。
    fn new(
        face: Face,
        u: f32,
        o3: (f32, f32, f32),
        ep: (f32, f32, f32),
        eq: (f32, f32, f32),
    ) -> Option<Self> {
        let o = project(o3.0, o3.1, o3.2, u);
        let p1 = project(o3.0 + ep.0, o3.1 + ep.1, o3.2 + ep.2, u);
        let q1 = project(o3.0 + eq.0, o3.1 + eq.1, o3.2 + eq.2, u);
        let (m00, m10) = (p1.0 - o.0, p1.1 - o.1);
        let (m01, m11) = (q1.0 - o.0, q1.1 - o.1);
        let det = m00 * m11 - m01 * m10;
        if det.abs() < 1e-6 {
            return None; // 退化成一条线，不可见
        }
        Some(Self { face, o, inv: [m11 / det, -m01 / det, -m10 / det, m00 / det] })
    }

    /// 命中测试。返回参数坐标 (p,q)，都在 [0,1] 内才算落在这个面上。
    #[inline]
    fn hit(&self, a: f32, b: f32) -> Option<(f32, f32)> {
        let (da, db) = (a - self.o.0, b - self.o.1);
        let p = self.inv[0] * da + self.inv[1] * db;
        let q = self.inv[2] * da + self.inv[3] * db;
        // 放宽半像素，避免相邻面之间因取整出现缝隙
        const E: f32 = 0.002;
        if (-E..=1.0 + E).contains(&p) && (-E..=1.0 + E).contains(&q) {
            Some((p.clamp(0.0, 1.0), q.clamp(0.0, 1.0)))
        } else {
            None
        }
    }
}

/// 预先算好的方块精灵：每个像素属于哪个面、贴图坐标、是不是边缘。
///
/// 按 (u, 形状) 缓存复用——逐像素现解仿射逆变换会是整个渲染的热点。
struct BoxSprite {
    size: usize,
    face: Vec<Face>,
    /// 面内归一化参数 (p, q)，都在 0..1。
    ///
    /// 刻意**不**在这里算贴图坐标：同一个包围盒的不同面可以有各自的 uv
    /// 取样矩形（火把的顶面就跟侧面取不同区域），把 uv 留到绘制时再套，
    /// 精灵才能按包围盒缓存复用。
    param: Vec<(f32, f32)>,
    edge: Vec<bool>,
}

impl BoxSprite {
    /// `shape` 是 1/16 单位的包围盒 `[fx,fy,fz,tx,ty,tz]`，**可能伸出 0..16 之外**
    /// （活塞头的杆、墙上火把）。精灵只有 2u×2u，落在格子外的部分会被下面的
    /// `for b in 0..size` 直接漏掉——也就是自动裁到格内，和以前的表现一样。
    /// 等距缩略图上一个方块才几像素，这点截断看不出来；
    /// 3D 视图不受这个限制，那边按真实包围盒画。
    ///
    /// 三个可见面（顶、+Z、+X）各建一个平行四边形并求逆；
    /// 凸长方体的这三个面在投影上互不重叠，逐像素依次命中测试即可。
    /// 贴图坐标直接取自方块局部坐标——这正是 MC 对未显式指定 `uv`
    /// 的面所用的规则，所以台阶的侧面自然只取贴图的下半部分。
    fn new(u: i32, shape: [i8; 6]) -> Self {
        let uf = u as f32;
        let s = shape.map(|v| v as f32 / 16.0);
        let (fx, fy, fz, tx, ty, tz) = (s[0], s[1], s[2], s[3], s[4], s[5]);
        let (dx, dy, dz) = (tx - fx, ty - fy, tz - fz);

        let mut patches = Vec::with_capacity(3);
        // 顶面 (Y = ty)：参数 p 沿 +X，q 沿 +Z
        patches.extend(FacePatch::new(
            Face::Top,
            uf,
            (fx, ty, fz),
            (dx, 0.0, 0.0),
            (0.0, 0.0, dz),
        ));
        // +X 面（画面右半）：p 沿 +Z，q 沿 -Y
        patches.extend(FacePatch::new(
            Face::Right,
            uf,
            (tx, ty, fz),
            (0.0, 0.0, dz),
            (0.0, -dy, 0.0),
        ));
        // +Z 面（画面左半）：p 沿 +X，q 沿 -Y
        patches.extend(FacePatch::new(
            Face::Left,
            uf,
            (fx, ty, tz),
            (dx, 0.0, 0.0),
            (0.0, -dy, 0.0),
        ));

        let size = (2 * u) as usize;
        let mut face = vec![Face::None; size * size];
        let mut param = vec![(0.0f32, 0.0f32); size * size];
        for b in 0..size {
            for a in 0..size {
                // 取像素中心，避免整块偏移半个像素
                let (af, bf) = (a as f32 + 0.5, b as f32 + 0.5);
                for patch in &patches {
                    let Some((p, q)) = patch.hit(af, bf) else { continue };
                    let k = b * size + a;
                    face[k] = patch.face;
                    param[k] = (p, q);
                    break;
                }
            }
        }

        // 边缘 = 四邻域里出现了不同的面（含框外的 None）
        let mut edge = vec![false; size * size];
        for b in 0..size {
            for a in 0..size {
                let f = face[b * size + a];
                if f == Face::None {
                    continue;
                }
                let mut diff = false;
                for (da, db) in [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)] {
                    let na = a as i32 + da;
                    let nb = b as i32 + db;
                    let nf = if na < 0 || nb < 0 || na >= size as i32 || nb >= size as i32 {
                        Face::None
                    } else {
                        face[nb as usize * size + na as usize]
                    };
                    if nf != f {
                        diff = true;
                        break;
                    }
                }
                edge[b * size + a] = diff;
            }
        }
        Self { size, face, param, edge }
    }
}

/// 面内参数按 90 度的整数倍旋转。红石线的走向就是靠这个转出来的。
#[inline]
fn spin(p: f32, q: f32, rot: u8) -> (f32, f32) {
    match rot & 3 {
        1 => (q, 1.0 - p),
        2 => (1.0 - p, 1.0 - q),
        3 => (1.0 - q, p),
        _ => (p, q),
    }
}

/// 面内参数 + 该面的 uv 取样矩形与旋转 → 贴图纹素坐标。
///
/// uv 矩形可能是反的（红石粉的 down 面写着 `[0,16,16,0]`），直接线性插值即可。
#[inline]
fn texel_of(p: f32, q: f32, rect: [u8; 4], rot: u8, tile: u32) -> (u32, u32) {
    let (p, q) = spin(p, q, rot);
    let t = tile as f32 / 16.0;
    let x = (rect[0] as f32 + p * (rect[2] as f32 - rect[0] as f32)) * t;
    let y = (rect[1] as f32 + q * (rect[3] as f32 - rect[1] as f32)) * t;
    let m = tile as f32 - 0.001;
    (x.clamp(0.0, m) as u32, y.clamp(0.0, m) as u32)
}

/// 把带 alpha 的纹素混合到已有像素上。
///
/// 画家算法是由远及近画的，所以「已有像素」就是这个方块背后的东西——
/// 红石线的透明部分因此能正确透出它脚下的方块。
/// 背后什么都没有（还是背景）时退回压暗的平均色，
/// 免得漏斗顶那种中间镂空的贴图在方块上开个洞、直接透出画布。
#[inline]
fn blend(dst: Rgba<u8>, texel: [u8; 4], behind: Rgb) -> Option<Rgba<u8>> {
    let a = texel[3] as u32;
    if a == 0 {
        return None;
    }
    if a == 255 {
        return Some(Rgba([texel[0], texel[1], texel[2], 255]));
    }
    let base = if dst.0[3] == 0 {
        [
            (behind[0] as u32 * 40 / 100) as u8,
            (behind[1] as u32 * 40 / 100) as u8,
            (behind[2] as u32 * 40 / 100) as u8,
        ]
    } else {
        [dst.0[0], dst.0[1], dst.0[2]]
    };
    let mut out = [0u8; 4];
    for i in 0..3 {
        out[i] = ((texel[i] as u32 * a + base[i] as u32 * (255 - a)) / 255) as u8;
    }
    out[3] = 255;
    Some(Rgba(out))
}

#[inline]
fn shade(c: Rgb, num: u32, den: u32) -> Rgba<u8> {
    Rgba([
        ((c[0] as u32 * num) / den).min(255) as u8,
        ((c[1] as u32 * num) / den).min(255) as u8,
        ((c[2] as u32 * num) / den).min(255) as u8,
        255,
    ])
}

/// 等距缩略图。`atlas` 为 None 时退回平均色填充。
pub fn isometric(
    schem: &Schematic,
    assets: &BlockAssets,
    atlas: Option<&Atlas>,
    opts: &RenderOptions,
) -> Result<RgbaImage> {
    let grid = VoxelGrid::build(schem, assets, atlas, opts)
        .ok_or_else(|| anyhow::anyhow!("蓝图没有任何 region"))?;
    Ok(isometric_grid(&grid, atlas, opts))
}

pub fn isometric_grid(grid: &VoxelGrid, atlas: Option<&Atlas>, opts: &RenderOptions) -> RgbaImage {
    let ss = opts.supersample.clamp(1, 4);
    let hi = render_iso(grid, atlas, opts, opts.max_px * ss);
    if ss == 1 {
        return hi;
    }
    downsample(&hi, ss)
}

fn render_iso(
    grid: &VoxelGrid,
    atlas: Option<&Atlas>,
    opts: &RenderOptions,
    max_px: u32,
) -> RgbaImage {
    let (w, h, d) = (grid.w as i64, grid.h as i64, grid.d as i64);

    // 屏幕范围：px = (x - z) * u，py = (x + z) * u/2 - y * u，精灵框 2u×2u
    let span_x = (w + d - 1).max(1) as u32;
    let span_y = ((w + d - 2).max(0) as u32).div_ceil(2) + h as u32 + 1;

    let u = (max_px / span_x).min(max_px / span_y.max(1)).clamp(2, 64) as i32;
    // u 必须是偶数，否则顶面菱形的半高 u/2 取整会让相邻方块错位一像素
    let u = if u % 2 == 0 { u } else { u - 1 }.max(2);

    // 方块在屏幕上太小时贴材质只会变成噪点，不如用平均色
    let tile = atlas.map_or(16, |a| a.tile_size);
    let use_texture = atlas.is_some() && u >= opts.min_texture_px;

    // 形状种类远少于方块数（一个蓝图通常几十种），按包围盒缓存精灵
    const FULL: [i8; 6] = [0, 0, 0, 16, 16, 16];
    let mut sprites: HashMap<[i8; 6], BoxSprite> = HashMap::new();
    sprites.insert(FULL, BoxSprite::new(u, FULL));
    for b in &grid.palette {
        for bx in &b.boxes {
            sprites.entry(bx.bbox).or_insert_with(|| BoxSprite::new(u, bx.bbox));
        }
    }

    let img_w = (span_x as i32 * u + u) as u32;
    let img_h = (span_y as i32 * u + u) as u32;

    let mut img = RgbaImage::from_pixel(
        img_w.max(1),
        img_h.max(1),
        match opts.background {
            Some(c) => Rgba([c[0], c[1], c[2], 255]),
            None => Rgba([0, 0, 0, 0]),
        },
    );

    let off_x = (d as i32 - 1) * u;
    let off_y = (h as i32 - 1) * u;
    let edge_num = 100u32.saturating_sub(opts.edge_strength.min(60));

    // 画家算法：沿视线方向 (1,1,1) 由远及近，即按 x+y+z 升序绘制。
    // 用桶排序代替真排序——键的范围是已知的小整数。
    let max_key = (w + h + d - 3).max(0) as usize;
    let mut buckets: Vec<Vec<(u16, u16, u16)>> = vec![Vec::new(); max_key + 1];
    for y in 0..grid.h {
        for z in 0..grid.d {
            for x in 0..grid.w {
                if grid.is_solid(x, y, z) {
                    buckets[x + y + z].push((x as u16, y as u16, z as u16));
                }
            }
        }
    }

    let fallback = VoxelBlock {
        name: String::new(),
        top: opts.fallback,
        side: opts.fallback,
        boxes: Vec::new(),
        opacity: Opacity::Opaque,
        repr: (NO_TILE, NO_TILE),
    };

    for bucket in &buckets {
        for &(x, y, z) in bucket {
            let block = grid.block_at(x as usize, y as usize, z as usize).unwrap_or(&fallback);
            // 降采样时一个体素代表多个方块，此时按完整方块画才合理。
            // **但六面要贴这个方块的代表图块，不能退回平均色**——
            // 退回平均色的话，只要蓝图最长边超过 target_grid，整张缩略图就全是色块。
            let flat_box;
            let boxes: &[ResolvedBox] = if grid.scale > 1 || block.boxes.is_empty() {
                flat_box = block.repr_box();
                std::slice::from_ref(&flat_box)
            } else {
                &block.boxes
            };

            let px = (x as i32 - z as i32) * u + off_x;
            let py = (x as i32 + z as i32) * u / 2 - y as i32 * u + off_y;

            // 一格内的多个长方体也要由远及近，否则楼梯的踏面会被底座盖住
            let mut order: Vec<&ResolvedBox> = boxes.iter().collect();
            order.sort_by_key(|b| {
                (b.bbox[0] as i32 + b.bbox[3] as i32)
                    + (b.bbox[1] as i32 + b.bbox[4] as i32)
                    + (b.bbox[2] as i32 + b.bbox[5] as i32)
            });

            for bx in order {
                let Some(sprite) = sprites.get(&bx.bbox) else { continue };
                for b in 0..sprite.size {
                    let iy = py + b as i32;
                    if iy < 0 || iy as u32 >= img_h {
                        continue;
                    }
                    for a in 0..sprite.size {
                        let ix = px + a as i32;
                        if ix < 0 || ix as u32 >= img_w {
                            continue;
                        }
                        let k = b * sprite.size + a;
                        // 顶面全亮，左右面依次压暗，立体感就靠这个。
                        // Left 是 +Z（南）面，Right 是 +X（东）面。
                        let (avg, fi, num) = match sprite.face[k] {
                            Face::None => continue,
                            Face::Top => (block.top, mcassets::model::UP, 100),
                            Face::Left => (block.side, mcassets::model::SOUTH, 78),
                            Face::Right => (block.side, mcassets::model::EAST, 58),
                        };

                        // 模型没声明这一面就不画。退回平均色只是把「带纹理的错东西」
                        // 换成「纯色的错东西」——红石线侧面那个小红点照样在
                        if !mcassets::model::face_drawn(bx.faces[fi]) {
                            continue;
                        }

                        let num = if sprite.edge[k] { num * edge_num / 100 } else { num };

                        let base = if use_texture {
                            let (p, q) = sprite.param[k];
                            let (tx, ty) = texel_of(p, q, bx.uv[fi], bx.rot[fi], tile);
                            match atlas.and_then(|at| at.texel(bx.faces[fi], tx, ty)) {
                                Some(t) => {
                                    let dst = *img.get_pixel(ix as u32, iy as u32);
                                    // 全透明的纹素直接跳过，让背后的方块透出来
                                    let Some(c) = blend(dst, t, avg) else { continue };
                                    [c.0[0], c.0[1], c.0[2]]
                                }
                                None => avg,
                            }
                        } else {
                            avg
                        };

                        img.put_pixel(ix as u32, iy as u32, shade(base, num, 100));
                    }
                }
            }
        }
    }
    img
}

/// 盒式降采样。超采样出来的图缩回目标尺寸，边缘的锯齿就变成了灰阶过渡。
fn downsample(src: &RgbaImage, factor: u32) -> RgbaImage {
    let f = factor.max(1);
    let (sw, sh) = src.dimensions();
    let dw = (sw / f).max(1);
    let dh = (sh / f).max(1);
    let mut out = RgbaImage::new(dw, dh);
    for y in 0..dh {
        for x in 0..dw {
            let (mut r, mut g, mut b, mut a) = (0u32, 0u32, 0u32, 0u32);
            for dy in 0..f {
                for dx in 0..f {
                    let p = src.get_pixel((x * f + dx).min(sw - 1), (y * f + dy).min(sh - 1)).0;
                    // 按 alpha 加权，否则透明背景会把边缘像素往黑里拉
                    let w = p[3] as u32;
                    r += p[0] as u32 * w;
                    g += p[1] as u32 * w;
                    b += p[2] as u32 * w;
                    a += w;
                }
            }
            let n = f * f;
            out.put_pixel(
                x,
                y,
                if a == 0 {
                    Rgba([0, 0, 0, 0])
                } else {
                    Rgba([(r / a) as u8, (g / a) as u8, (b / a) as u8, (a / n) as u8])
                },
            );
        }
    }
    out
}

// ---------------------------------------------------------------- Y 轴切片

#[derive(Clone, Copy)]
struct FlatCell {
    color: Rgb,
    tile: u16,
    /// 顶面的贴图取样矩形
    uv: [u8; 4],
    /// 顶面的纹理旋转
    rot: u8,
    filled: bool,
}

impl Default for FlatCell {
    fn default() -> Self {
        Self { color: [0, 0, 0], tile: NO_TILE, uv: [0, 0, 16, 16], rot: 0, filled: false }
    }
}

/// 单层俯视图。生电蓝图看内部电路全靠这个。
///
/// `y` 是相对包围盒底部的层号（0 = 最底层）。
///
/// 只解出这一层，内存 O(宽×深)。**不要**改成先建完整 3D 网格再取一层：
/// `流萤.litematic` 有 5.15 亿方块，完整网格要好几 GB。
pub fn slice_top_down(
    schem: &Schematic,
    assets: &BlockAssets,
    atlas: Option<&Atlas>,
    y: usize,
    cell_px: u32,
    opts: &RenderOptions,
) -> Result<RgbaImage> {
    let (lo, hi) = schem
        .bounding_box()
        .ok_or_else(|| anyhow::anyhow!("蓝图没有任何 region"))?;
    let w = (hi.x - lo.x + 1).max(1) as usize;
    let d = (hi.z - lo.z + 1).max(1) as usize;
    let h = (hi.y - lo.y + 1).max(1) as usize;

    let mut cells = vec![FlatCell::default(); w * d];
    if y < h {
        let target_y = lo.y + y as i32;
        for region in &schem.regions {
            let min = region.min_corner();
            let ext = region.extent();
            let ly = target_y - min.y;
            if ly < 0 || ly >= ext.y {
                continue;
            }
            let pal: Vec<Option<(Rgb, (u16, [u8; 4], u8))>> = region
                .palette
                .iter()
                .map(|bs| {
                    if bs.is_air() {
                        None
                    } else {
                        let c = assets.get(&bs.name).map_or(opts.fallback, |c| c.top);
                        // 俯视切片只关心朝上的那一面：取最高的那个长方体的顶面贴图。
                        // 只在**真的画顶面**的长方体里挑——红石火把的辉光片、
                        // 红石线都有只声明单面的 element，挑中它们会拿到空贴图。
                        let boxes = assets.resolve(&bs.name, &bs.properties);
                        let pick = boxes
                            .iter()
                            .filter(|b| mcassets::model::face_drawn(b.faces[mcassets::model::UP]))
                            .max_by_key(|b| b.bbox[4])
                            .or_else(|| boxes.iter().max_by_key(|b| b.bbox[4]));
                        // 切片是张俯视地图，有方块就得看得见；顶面没声明时退回平均色，
                        // 不像等距那样整个跳过
                        let t = pick.map_or((NO_TILE, [0, 0, 16, 16], 0u8), |b| {
                            let f = b.faces[mcassets::model::UP];
                            (
                                if mcassets::model::face_drawn(f) { f } else { NO_TILE },
                                b.uv[mcassets::model::UP],
                                b.rot[mcassets::model::UP],
                            )
                        });
                        Some((c, t))
                    }
                })
                .collect();

            let (sx, sz) = (ext.x.max(1) as u64, ext.z.max(1) as u64);
            let plane = sx * sz;
            // Litematica 的索引是 y 最外层，所以这一层就是一段连续区间
            let start = ly as u64 * plane;
            region.for_each_block_range(start, plane, |index, pi| {
                let Some(Some((color, (tile, uv, rot)))) = pal.get(pi as usize) else { return };
                let local = index - start;
                let lz = local / sx;
                let lx = local % sx;
                let gx = (min.x as i64 + lx as i64 - lo.x as i64) as usize;
                let gz = (min.z as i64 + lz as i64 - lo.z as i64) as usize;
                if gx < w && gz < d {
                    cells[gz * w + gx] =
                        FlatCell { color: *color, tile: *tile, uv: *uv, rot: *rot, filled: true };
                }
            });
        }
    }

    Ok(draw_layer(&cells, w, d, cell_px, atlas, opts))
}

/// 每层的非空气方块数。内存 O(层数)，同样不建完整网格。
pub fn layer_counts(schem: &Schematic) -> Option<Vec<u64>> {
    let (lo, hi) = schem.bounding_box()?;
    let h = (hi.y - lo.y + 1).max(1) as usize;
    let mut counts = vec![0u64; h];

    for region in &schem.regions {
        let min = region.min_corner();
        let ext = region.extent();
        let non_air: Vec<bool> = region.palette.iter().map(|bs| !bs.is_air()).collect();
        let (sx, sz) = (ext.x.max(1) as u64, ext.z.max(1) as u64);
        let plane = sx * sz;

        for ly in 0..ext.y.max(0) {
            let gy = (min.y + ly - lo.y) as usize;
            if gy >= h {
                continue;
            }
            let mut n = 0u64;
            region.for_each_block_range(ly as u64 * plane, plane, |_, pi| {
                if non_air.get(pi as usize).copied().unwrap_or(false) {
                    n += 1;
                }
            });
            counts[gy] += n;
        }
    }
    Some(counts)
}

fn draw_layer(
    cells: &[FlatCell],
    w: usize,
    d: usize,
    cell_px: u32,
    atlas: Option<&Atlas>,
    opts: &RenderOptions,
) -> RgbaImage {
    let cell = cell_px.max(1);
    // 一个方块只有几像素时贴材质会糊成噪点
    let textured = atlas.is_some() && cell as i32 >= opts.min_texture_px;
    let tile = atlas.map_or(16, |a| a.tile_size);

    let mut img = RgbaImage::from_pixel(
        (w as u32 * cell).max(1),
        (d as u32 * cell).max(1),
        match opts.background {
            Some(c) => Rgba([c[0], c[1], c[2], 255]),
            None => Rgba([0, 0, 0, 0]),
        },
    );
    for z in 0..d {
        for x in 0..w {
            let c = cells[z * w + x];
            if !c.filled {
                continue;
            }
            for dy in 0..cell {
                for dx in 0..cell {
                    let base = if textured {
                        let (tx, ty) = texel_of(
                            dx as f32 / cell as f32,
                            dy as f32 / cell as f32,
                            c.uv,
                            c.rot,
                            tile,
                        );
                        match atlas.and_then(|a| a.texel(c.tile, tx, ty)) {
                            // 切片是俯视图，透明处混到压暗的平均色上
                            Some(t) => match blend(Rgba([0, 0, 0, 0]), t, c.color) {
                                Some(v) => [v.0[0], v.0[1], v.0[2]],
                                None => continue,
                            },
                            None => c.color,
                        }
                    } else {
                        c.color
                    };
                    // 格子够大时留 1px 缝，方块边界才看得清
                    let gap = cell >= 4 && (dx == cell - 1 || dy == cell - 1);
                    let v = if gap { shade(base, 62, 100) } else { shade(base, 100, 100) };
                    img.put_pixel(x as u32 * cell + dx, z as u32 * cell + dy, v);
                }
            }
        }
    }
    img
}

#[cfg(test)]
mod tests {
    use super::*;
    const FULL_CUBE: [i8; 6] = [0, 0, 0, 16, 16, 16];

    fn sprite_faces(shape: [i8; 6], u: i32) -> [usize; 4] {
        let s = BoxSprite::new(u, shape);
        let mut counts = [0usize; 4];
        for f in &s.face {
            counts[match f {
                Face::None => 0,
                Face::Top => 1,
                Face::Left => 2,
                Face::Right => 3,
            }] += 1;
        }
        counts
    }

    /// 造一张 16×16 的单图块图集，按给定的 alpha 序列铺满
    fn one_tile_atlas(alphas: &[u8]) -> Atlas {
        let mut img = RgbaImage::new(16, 16);
        for (i, p) in img.pixels_mut().enumerate() {
            *p = Rgba([200, 200, 200, alphas[i % alphas.len()]]);
        }
        Atlas::new(img, 16, 1)
    }

    #[test]
    fn tile_opacity_classifies_by_alpha() {
        assert_eq!(one_tile_atlas(&[255]).tile_class(0), Opacity::Opaque, "全不透");
        // 原版玻璃：中间全透、边框全不透
        assert_eq!(one_tile_atlas(&[0, 255]).tile_class(0), Opacity::Cutout, "半数全透");
        // 染色玻璃：每个纹素都是半透的，一个都不会被 alphaTest 剪掉
        assert_eq!(one_tile_atlas(&[128]).tile_class(0), Opacity::Translucent, "全半透");
        // 半透只要有一个就得算 Translucent——alphaTest 处理不了它
        let mut mostly = vec![255u8; 256];
        mostly[7] = 128;
        assert_eq!(one_tile_atlas(&mostly).tile_class(0), Opacity::Translucent, "混一个半透");
    }

    /// 贴图边缘的一两个杂点不该让整个方块不再遮挡——那会白白多出一堆体素。
    #[test]
    fn stray_transparent_pixels_do_not_make_a_cutout() {
        let mut a = vec![255u8; 256];
        a[0] = 0;
        a[1] = 0;
        a[2] = 0;
        assert_eq!(one_tile_atlas(&a).tile_class(0), Opacity::Opaque, "3/256 不到 1/64");
        a[3] = 0;
        assert_eq!(one_tile_atlas(&a).tile_class(0), Opacity::Cutout, "4/256 正好 1/64");
    }

    #[test]
    fn no_tile_counts_as_opaque() {
        // 没有图集图块的方块退回平均色画实心立方体，那就该挡得住邻居
        assert_eq!(one_tile_atlas(&[255]).tile_class(NO_TILE), Opacity::Opaque);
    }

    fn block(boxes: Vec<ResolvedBox>, opacity: Opacity) -> VoxelBlock {
        let repr = repr_tiles(&boxes);
        VoxelBlock { name: String::new(), top: [0; 3], side: [0; 3], boxes, opacity, repr }
    }

    fn full_box() -> ResolvedBox {
        ResolvedBox {
            bbox: FULL_CUBE,
            faces: [NO_TILE; 6],
            uv: mcassets::model::default_uv(FULL_CUBE),
            rot: [0; 6],
        }
    }

    /// 核心回归：玻璃是完整立方体，但**看得穿，不能当遮挡物**。
    ///
    /// 只按 `is_full_cube` 剔除的话，玻璃罩里的整台机器会从 3D 视图里整个消失，
    /// 而且无论着色器怎么改都救不回来——数据根本没送到前端。
    #[test]
    fn see_through_full_cubes_do_not_occlude() {
        let stone = block(vec![full_box()], Opacity::Opaque);
        let glass = block(vec![full_box()], Opacity::Cutout);
        let stained = block(vec![full_box()], Opacity::Translucent);
        // 火把之类：本来就不填满一格
        let torch = block(
            vec![ResolvedBox {
                bbox: [7, 0, 7, 9, 10, 9],
                faces: [NO_TILE; 6],
                uv: mcassets::model::default_uv([7, 0, 7, 9, 10, 9]),
                rot: [0; 6],
            }],
            Opacity::Opaque,
        );

        assert!(stone.blocks_sight(), "石头该挡住邻居");
        assert!(!glass.blocks_sight(), "玻璃填满一格但看得穿");
        assert!(!stained.blocks_sight(), "染色玻璃同理");
        assert!(!torch.blocks_sight(), "火把没填满一格");

        // is_full_cube 仍然只看几何——两者不是一回事，别合并
        assert!(glass.is_full_cube() && stained.is_full_cube());
    }

    fn one_block_grid(faces: [u16; 6]) -> VoxelGrid {
        let bx = ResolvedBox {
            bbox: FULL_CUBE,
            faces,
            uv: mcassets::model::default_uv(FULL_CUBE),
            rot: [0; 6],
        };
        let mut cells = vec![Cell::default(); 1];
        cells[0].vote(0);
        VoxelGrid {
            w: 1,
            h: 1,
            d: 1,
            scale: 1,
            origin: Vec3i::default(),
            palette: vec![block(vec![bx], Opacity::Opaque)],
            cells,
            fill_threshold: 1,
        }
    }

    fn painted_pixels(g: &VoxelGrid) -> usize {
        let opts = RenderOptions { background: None, supersample: 1, ..Default::default() };
        isometric_grid(g, None, &opts).pixels().filter(|p| p.0[3] > 0).count()
    }

    /// 关键回归：等距渲染必须跳过模型没声明的面。
    ///
    /// 红石线的 `redstone_dust_side` 只声明 up/down，以前四个侧面被顶上贴图，
    /// 在 1/16 厚的薄片侧面渲染成一个游离的小红点。
    /// 注意**退回平均色是不够的**——那只是把带纹理的错东西换成纯色的错东西，
    /// 点还在。所以这里断言的是「一个像素都不画」。
    #[test]
    fn isometric_skips_undeclared_faces() {
        let all = painted_pixels(&one_block_grid([NO_TILE; 6]));
        // 只声明顶面和底面，四个侧面没声明——和红石线一个形状
        let mut f = [NO_FACE; 6];
        f[mcassets::model::UP] = NO_TILE;
        f[mcassets::model::DOWN] = NO_TILE;
        let top_only = painted_pixels(&one_block_grid(f));

        assert!(all > 0, "六面都声明时得画出东西");
        assert!(top_only > 0, "顶面声明了就该画出来");
        // 等距只画 顶/南/东 三个面，去掉两个侧面后应该只剩约三分之一
        assert!(
            top_only * 2 < all,
            "侧面没声明却还画了: 顶面 {top_only} px vs 六面 {all} px"
        );
    }

    #[test]
    fn block_class_takes_the_most_see_through_face() {
        let at = one_tile_atlas(&[128]);
        let opaque_tile = one_tile_atlas(&[255]);
        // 全 NO_TILE 的方块没有任何贴图信息，按不透明算
        assert_eq!(opaque_tile.block_class(&[full_box()]), Opacity::Opaque);
        // 只要有一个面落在半透图块上，整个方块就得走混合那一趟
        let mut b = full_box();
        b.faces[2] = 0;
        assert_eq!(at.block_class(&[b]), Opacity::Translucent);
    }

    #[test]
    fn full_cube_covers_hexagon_with_all_three_faces() {
        for u in [4, 8, 12, 16] {
            let c = sprite_faces(FULL_CUBE, u);
            let covered = c[1] + c[2] + c[3];
            let hexagon = 3 * (u * u) as usize;
            assert!(
                covered as f64 > hexagon as f64 * 0.9 && (covered as f64) < hexagon as f64 * 1.1,
                "u={u}: 覆盖 {covered} 像素，六边形理论面积 {hexagon}"
            );
            assert!(c[1] > 0 && c[2] > 0 && c[3] > 0, "u={u}: 有面完全没画出来");
        }
    }

    #[test]
    fn full_cube_left_and_right_faces_are_symmetric() {
        let c = sprite_faces(FULL_CUBE, 16);
        assert_eq!(c[2], c[3], "左右面像素数应对称");
    }

    /// 关键回归：非完整方块必须画得比完整方块小。
    /// 之前火把、活板门、栅栏全被画成 1×1×1 的立方体。
    #[test]
    fn partial_shapes_cover_less_than_full_cube() {
        let u = 16;
        let full = sprite_faces(FULL_CUBE, u);
        let full_area = full[1] + full[2] + full[3];

        // 活板门：16×3×16
        let trapdoor = sprite_faces([0, 0, 0, 16, 3, 16], u);
        let td_area = trapdoor[1] + trapdoor[2] + trapdoor[3];
        assert!(td_area < full_area, "活板门应比完整方块小: {td_area} vs {full_area}");
        assert!(trapdoor[1] > 0, "活板门应有顶面");

        // 火把：2×10×2，很细
        let torch = sprite_faces([7, 0, 7, 9, 10, 9], u);
        let torch_area = torch[1] + torch[2] + torch[3];
        assert!(torch_area < full_area / 4, "火把应远小于完整方块: {torch_area} vs {full_area}");

        // 栅栏柱：4×16×4
        let fence = sprite_faces([6, 0, 6, 10, 16, 10], u);
        let fence_area = fence[1] + fence[2] + fence[3];
        assert!(fence_area < full_area / 2, "栅栏柱应明显细于完整方块");
        assert!(fence[2] > 0 && fence[3] > 0, "栅栏柱应有两个侧面");

        // 台阶：16×8×16，约为完整方块的一半多
        let slab = sprite_faces([0, 0, 0, 16, 8, 16], u);
        let slab_area = slab[1] + slab[2] + slab[3];
        assert!(slab_area < full_area && slab_area > full_area / 3, "台阶面积应在合理区间");
        assert_eq!(slab[1], full[1], "台阶顶面与完整方块顶面一样大");
    }

    /// 台阶的侧面应当只取贴图的下半部分——这是 MC 对未显式指定 uv 的面的规则。
    #[test]
    fn partial_shape_uv_follows_block_local_coords() {
        let shape = [0i8, 0, 0, 16, 8, 16];
        let uv = mcassets::model::default_uv(shape);
        let s = BoxSprite::new(32, shape);
        let mut side_v = Vec::new();
        for (k, f) in s.face.iter().enumerate() {
            let rect = match f {
                Face::Left => uv[mcassets::model::SOUTH],
                Face::Right => uv[mcassets::model::EAST],
                _ => continue,
            };
            let (p, q) = s.param[k];
            side_v.push(texel_of(p, q, rect, 0, 16).1);
        }
        assert!(!side_v.is_empty());
        let lo = *side_v.iter().min().unwrap();
        assert!(lo >= 7, "下半台阶的侧面贴图应从中线以下开始取，实际最小 v={lo}");
    }

    /// 显式 uv 必须真的换到另一块取样区。火把的顶面就靠它拿到火焰。
    #[test]
    fn explicit_uv_overrides_derived_region() {
        let shape = [7i8, 0, 7, 9, 10, 9];
        let s = BoxSprite::new(48, shape);
        let derived = mcassets::model::default_uv(shape)[mcassets::model::UP];
        let explicit = [7u8, 6, 9, 8];
        assert_ne!(derived, explicit, "这个例子本身要有区别才有意义");

        let mut got_derived = Vec::new();
        let mut got_explicit = Vec::new();
        for (k, f) in s.face.iter().enumerate() {
            if *f != Face::Top {
                continue;
            }
            let (p, q) = s.param[k];
            got_derived.push(texel_of(p, q, derived, 0, 16));
            got_explicit.push(texel_of(p, q, explicit, 0, 16));
        }
        assert!(!got_derived.is_empty());
        assert_ne!(got_derived, got_explicit, "显式 uv 应当取到不同的纹素");
        // 显式区域是 v=6..8，推导区域是 v=7..9
        assert!(got_explicit.iter().all(|(_, v)| (6..8).contains(v)), "显式 uv 应落在 v=6..8");
    }

    #[test]
    fn sprite_marks_outline_but_not_interior() {
        let s = BoxSprite::new(8, FULL_CUBE);
        let k = (8 / 2) * s.size + 8;
        assert_eq!(s.face[k], Face::Top);
        assert!(!s.edge[k], "面的中心不该被标为边缘");
        assert!(s.edge.iter().filter(|e| **e).count() > 8 * 4, "描边像素太少");
    }

    /// 贴图坐标必须始终落在图块内，越界会采样到邻居图块。
    #[test]
    fn uv_never_escapes_tile() {
        // 含反向 uv（红石粉的 down 面写着 [0,16,16,0]）也不能越界
        let rects: [[u8; 4]; 3] = [[0, 0, 16, 16], [7, 6, 9, 8], [0, 16, 16, 0]];
        for u in [4, 8, 16, 32] {
            for shape in [FULL_CUBE, [0, 0, 0, 16, 3, 16], [7, 0, 7, 9, 10, 9], [6, 0, 6, 10, 16, 10]] {
                let s = BoxSprite::new(u, shape);
                for (k, f) in s.face.iter().enumerate() {
                    if *f == Face::None {
                        continue;
                    }
                    let (p, q) = s.param[k];
                    assert!((0.0..=1.0).contains(&p) && (0.0..=1.0).contains(&q),
                            "u={u} shape={shape:?} 参数越界 ({p},{q})");
                    for r in rects {
                        let (tx, ty) = texel_of(p, q, r, 0, 16);
                        assert!(tx < 16 && ty < 16, "u={u} shape={shape:?} uv={r:?} -> ({tx},{ty}) 越界");
                    }
                }
            }
        }
    }

    #[test]
    fn atlas_texel_lookup() {
        let mut img = RgbaImage::new(32, 32);
        // 1 号图块（右上）整块涂红
        for y in 0..16 {
            for x in 16..32 {
                img.put_pixel(x, y, Rgba([255, 0, 0, 255]));
            }
        }
        let a = Atlas::new(img, 16, 2);
        assert_eq!(a.texel(1, 0, 0), Some([255, 0, 0, 255]));
        assert_eq!(a.texel(0, 0, 0), Some([0, 0, 0, 0]));
        assert_eq!(a.texel(NO_TILE, 0, 0), None);
        // 越界的图块坐标会被夹回图块内，不会读到邻居
        assert_eq!(a.texel(1, 99, 99), Some([255, 0, 0, 255]));
    }

    #[test]
    fn majority_vote_picks_dominant_block() {
        let mut c = Cell::default();
        for _ in 0..5 {
            c.vote(7);
        }
        for _ in 0..2 {
            c.vote(3);
        }
        assert_eq!(c.cand, 7);
        assert_eq!(c.count, 7);
    }

    #[test]
    fn single_block_cell_is_exact() {
        // scale=1 时一格就是一个方块，必须原样保留
        let mut c = Cell::default();
        c.vote(42);
        assert_eq!(c.cand, 42);
        assert_eq!(c.count, 1);
    }

    #[test]
    fn downsample_averages_and_keeps_alpha() {
        let mut src = RgbaImage::new(2, 2);
        src.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
        src.put_pixel(1, 0, Rgba([255, 0, 0, 255]));
        src.put_pixel(0, 1, Rgba([0, 0, 0, 0]));
        src.put_pixel(1, 1, Rgba([0, 0, 0, 0]));
        let out = downsample(&src, 2);
        assert_eq!(out.dimensions(), (1, 1));
        let p = out.get_pixel(0, 0).0;
        // 透明像素不该把颜色往黑里拉，但要让整体半透明
        assert_eq!([p[0], p[1], p[2]], [255, 0, 0]);
        assert_eq!(p[3], 127);
    }
}
