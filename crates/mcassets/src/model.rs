//! 方块模型：状态匹配、旋转、以及解析成一组带贴图的长方体。
//!
//! 之前的做法是「一个方块名 → 一张顶/侧/底贴图 + 一个包围盒」，
//! 这在生电蓝图上根本不够用：
//!
//! - 蓝图里每个方块都带着 `facing` / `half` / `axis` / `type` 等状态，
//!   一台机器里大半方块是有朝向的。只按方块名索引会让所有活塞都朝上、
//!   所有活板门都在下半格、所有梯子都贴同一面。
//! - 楼梯由两个 element 拼成，取并集就退化成完整立方体。
//! - 栅栏、墙、玻璃板是 multipart，只取第一个部件就只剩一根柱子。
//!
//! 所以这里保留 MC 模型系统的原始结构：blockstate 里的 variant / multipart
//! 规则指向模型，模型由若干带六面贴图的 element 组成，运行时按真实方块状态解析。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::NO_FACE;

/// 面的下标顺序，各处一致。
pub const UP: usize = 0;
pub const DOWN: usize = 1;
pub const NORTH: usize = 2;
pub const SOUTH: usize = 3;
pub const EAST: usize = 4;
pub const WEST: usize = 5;

/// 模型里的一个长方体。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ElementDef {
    /// 包围盒 `[fx,fy,fz,tx,ty,tz]`，单位 1/16
    /// 包围盒，单位 1/16 格。有符号且允许超出 0..16，见 [`ResolvedBox::bbox`]
    pub b: [i8; 6],
    /// 六个面的图集索引，顺序见 [`UP`] 等常量
    pub f: [u16; 6],
    /// 六个面各自的贴图取样矩形 `[u1,v1,u2,v2]`，单位 1/16。
    ///
    /// **不能只按包围盒推**。火把的元素在 x=7..9、y=0..10，按包围盒推顶面
    /// 取样区是 `[7,7,9,9]`，而模型里明写着 `[7,6,9,8]`——那才是火焰的位置。
    /// MC 允许每个面显式指定 uv，缺省时才按包围盒推。
    pub uv: [[u8; 4]; 6],
    /// 每个面的纹理旋转，单位是 90 度（0~3）。
    ///
    /// 红石线的走向就靠它：`redstone_dust_side0` 这类模型本身只画一条直线，
    /// 靠 multipart 的 `y` 旋转转到四个方向。只置换面而不旋转面内的贴图，
    /// 南北向和东西向的红石线会长得一模一样。
    pub rot: [u8; 6],
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelDef {
    pub e: Vec<ElementDef>,
}

/// blockstate 的 `variants` 条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantDef {
    /// 原始键，如 `"facing=north,half=top"`；空串表示无条件匹配
    pub w: String,
    /// 模型下标
    pub m: u32,
    #[serde(default)]
    pub x: i16,
    #[serde(default)]
    pub y: i16,
}

/// blockstate 的 `multipart` 条目。`w` 是「或」组，每组内是「与」。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartDef {
    #[serde(default)]
    pub w: Vec<Vec<(String, Vec<String>)>>,
    pub m: u32,
    #[serde(default)]
    pub x: i16,
    #[serde(default)]
    pub y: i16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BlockDef {
    /// 有序，取第一个匹配的
    #[serde(rename = "v")]
    Variants(Vec<VariantDef>),
    /// 所有匹配的部件叠加
    #[serde(rename = "p")]
    Multipart(Vec<PartDef>),
}

/// 解析结果：世界坐标下的一个长方体及其六面贴图。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedBox {
    /// 包围盒 `[fx,fy,fz,tx,ty,tz]`，单位 1/16 格。
    ///
    /// **有符号，而且允许超出 0..16。** MC 的模型本来就会伸出格子外：
    /// 活塞头的杆是 `from[6,6,4] to[10,10,20]`，最后 4 格**故意**插进活塞本体
    /// 那一格的凹槽里；墙上火把的 x 是 `-1..1`，扎进墙里半格。
    /// 以前这里是 `[u8;6]` 并把两端各自 clamp 到 0..16，杆就被截断在格子边界上，
    /// 于是活塞「杆和本体是分开的」——中间空出 4/16 格，而且杆的断口正对镜头
    /// （那一面模型没声明，不画，看起来是个空心管）。
    pub bbox: [i8; 6],
    pub faces: [u16; 6],
    /// 各面的贴图取样矩形 `[u1,v1,u2,v2]`，单位 1/16
    pub uv: [[u8; 4]; 6],
    /// 各面的纹理旋转（0~3，单位 90 度）
    pub rot: [u8; 6],
}

impl ResolvedBox {
    pub fn is_full_cube(&self) -> bool {
        self.bbox == [0, 0, 0, 16, 16, 16]
    }
}

// ---------------------------------------------------------------- 状态匹配

/// `"facing=north,half=top"` 里的每一项都必须与方块状态相符。
pub fn variant_matches(key: &str, props: &BTreeMap<String, String>) -> bool {
    if key.is_empty() {
        return true;
    }
    key.split(',').all(|term| {
        let Some((k, v)) = term.split_once('=') else { return true };
        // MC 允许值里用 `|` 表示「或」
        props.get(k).is_some_and(|actual| v.split('|').any(|x| x == actual))
    })
}

/// multipart 的 `when`：外层「或」，内层「与」，每个条件的值集合也是「或」。
pub fn part_matches(when: &[Vec<(String, Vec<String>)>], props: &BTreeMap<String, String>) -> bool {
    if when.is_empty() {
        return true; // 没有 when 就是无条件应用
    }
    when.iter().any(|group| {
        group.iter().all(|(k, vals)| {
            props.get(k).is_some_and(|actual| vals.iter().any(|v| v == actual))
        })
    })
}

// ---------------------------------------------------------------- 旋转

/// 绕 X 轴旋转后，世界的某个面对应模型的哪个面。
///
/// 方向由活塞钉死：`facing=up` 的 variant 是 `x:270`，而推头画在模型的
/// `north` 面上，所以 x=270 必须让 north 转到世界上方。
fn perm_x(x: i64) -> [usize; 6] {
    match x.rem_euclid(360) {
        90 => [SOUTH, NORTH, UP, DOWN, EAST, WEST],
        180 => [DOWN, UP, SOUTH, NORTH, EAST, WEST],
        270 => [NORTH, SOUTH, DOWN, UP, EAST, WEST],
        _ => [UP, DOWN, NORTH, SOUTH, EAST, WEST],
    }
}

fn faces_x(f: [u16; 6], x: i64) -> [u16; 6] {
    let p = perm_x(x);
    [f[p[0]], f[p[1]], f[p[2]], f[p[3]], f[p[4]], f[p[5]]]
}

/// 绕 Y 轴旋转后的面映射。
///
/// 方向由梯子钉死：`facing=east` 的 variant 是 `y:90`，而模型把梯子画在
/// `+Z`（南）面上；朝东的梯子挂在西墙，所以 y=90 必须把模型的南面转到世界西面。
fn perm_y(y: i64) -> [usize; 6] {
    match y.rem_euclid(360) {
        90 => [UP, DOWN, WEST, EAST, NORTH, SOUTH],
        180 => [UP, DOWN, SOUTH, NORTH, WEST, EAST],
        270 => [UP, DOWN, EAST, WEST, SOUTH, NORTH],
        _ => [UP, DOWN, NORTH, SOUTH, EAST, WEST],
    }
}

fn faces_y(f: [u16; 6], y: i64) -> [u16; 6] {
    let p = perm_y(y);
    [f[p[0]], f[p[1]], f[p[2]], f[p[3]], f[p[4]], f[p[5]]]
}

/// uv 属于面，面被旋转搬到哪，uv 就跟到哪。
fn uv_permute(uv: [[u8; 4]; 6], p: [usize; 6]) -> [[u8; 4]; 6] {
    [uv[p[0]], uv[p[1]], uv[p[2]], uv[p[3]], uv[p[4]], uv[p[5]]]
}

fn rot_permute(r: [u8; 6], p: [usize; 6]) -> [u8; 6] {
    [r[p[0]], r[p[1]], r[p[2]], r[p[3]], r[p[4]], r[p[5]]]
}

fn box_x(b: [i8; 6], x: i64) -> [i8; 6] {
    let (fx, fy, fz, tx, ty, tz) = (b[0], b[1], b[2], b[3], b[4], b[5]);
    match x.rem_euclid(360) {
        90 => [fx, fz, 16 - ty, tx, tz, 16 - fy],
        180 => [fx, 16 - ty, 16 - tz, tx, 16 - fy, 16 - fz],
        270 => [fx, 16 - tz, fy, tx, 16 - fz, ty],
        _ => b,
    }
}

fn box_y(b: [i8; 6], y: i64) -> [i8; 6] {
    let (fx, fy, fz, tx, ty, tz) = (b[0], b[1], b[2], b[3], b[4], b[5]);
    match y.rem_euclid(360) {
        90 => [16 - tz, fy, fx, 16 - fz, ty, tx],
        180 => [16 - tx, fy, 16 - tz, 16 - fx, ty, 16 - fz],
        270 => [fz, fy, 16 - tx, tz, ty, 16 - fx],
        _ => b,
    }
}

/// 把模型的一个 element 转到世界坐标。MC 的顺序是先绕 X 再绕 Y。
pub fn transform(e: &ElementDef, x: i64, y: i64) -> ResolvedBox {
    let bbox = box_y(box_x(e.b, x), y);
    let faces = faces_y(faces_x(e.f, x), y);
    let uv = uv_permute(uv_permute(e.uv, perm_x(x)), perm_y(y));
    let mut rot = rot_permute(rot_permute(e.rot, perm_x(x)), perm_y(y));
    // 绕 Y 转会连带把顶/底面的贴图转起来（侧面的贴图朝向不受影响）。
    // 红石线、铁轨的走向全靠这一步。
    let q = (y.rem_euclid(360) / 90) as u8;
    if q > 0 {
        rot[UP] = (rot[UP] + q) & 3;
        rot[DOWN] = (rot[DOWN] + 4 - q) & 3;
    }
    // 旋转后 from/to 可能颠倒，规范化一下。
    // 这里**不再往 0..16 里夹**——伸出格子外是合法的，见 ResolvedBox::bbox
    let mut b = bbox;
    for i in 0..3 {
        if b[i] > b[i + 3] {
            b.swap(i, i + 3);
        }
        if b[i] == b[i + 3] {
            b[i + 3] = b[i].saturating_add(1);
        }
    }
    ResolvedBox { bbox: b, faces, uv, rot }
}

/// 缺省 uv：按 MC 的规则从包围盒推。
/// 顶/底面取 x-z 平面，侧面取水平轴与翻转后的 y。
pub fn default_uv(b: [i8; 6]) -> [[u8; 4]; 6] {
    // 包围盒可能伸出格子外，而 uv 只有 0..16。夹一下——MC 那边是环绕采样，
    // 但那种面基本都在模型里显式写了 uv，走不到这个兜底
    let c = |v: i8| v.clamp(0, 16) as u8;
    let (fx, fy, fz, tx, ty, tz) = (c(b[0]), c(b[1]), c(b[2]), c(b[3]), c(b[4]), c(b[5]));
    let (vy0, vy1) = (16 - ty, 16 - fy);
    let mut uv = [[0u8; 4]; 6];
    uv[UP] = [fx, fz, tx, tz];
    uv[DOWN] = [fx, 16 - tz, tx, 16 - fz];
    uv[NORTH] = [16 - tx, vy0, 16 - fx, vy1];
    uv[SOUTH] = [fx, vy0, tx, vy1];
    uv[WEST] = [fz, vy0, tz, vy1];
    uv[EAST] = [16 - tz, vy0, 16 - fz, vy1];
    uv
}

/// 这一面该不该画。`NO_FACE` 表示模型压根没声明它——MC 不画，我们也不画。
///
/// 以前这里是 `fill_missing_faces`：拿同一 element 上任意一个有效面顶上，
/// 「免得出现空洞」。但两个渲染器对没贴图的面本来就会退回平均色，根本不会漏出洞；
/// 顶上去反而凭空造出了红石线的游离小红点和红石火把的一团红。详见 [`crate::NO_FACE`]。
#[inline]
pub fn face_drawn(tile: u16) -> bool {
    tile != NO_FACE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn variant_key_matching() {
        let p = props(&[("facing", "north"), ("half", "top"), ("open", "false")]);
        assert!(variant_matches("", &p), "空键应无条件匹配");
        assert!(variant_matches("facing=north", &p));
        assert!(variant_matches("facing=north,half=top", &p));
        assert!(!variant_matches("facing=south", &p));
        assert!(!variant_matches("facing=north,half=bottom", &p));
        // 值里的 | 是「或」
        assert!(variant_matches("facing=north|south", &p));
        assert!(!variant_matches("facing=east|west", &p));
    }

    #[test]
    fn multipart_when_matching() {
        let p = props(&[("north", "true"), ("south", "false")]);
        assert!(part_matches(&[], &p), "没有 when 就无条件应用");
        let north_only = vec![vec![("north".to_string(), vec!["true".to_string()])]];
        assert!(part_matches(&north_only, &p));
        let south_only = vec![vec![("south".to_string(), vec!["true".to_string()])]];
        assert!(!part_matches(&south_only, &p));
        // OR：任一组成立即可
        let either = vec![
            vec![("south".to_string(), vec!["true".to_string()])],
            vec![("north".to_string(), vec!["true".to_string()])],
        ];
        assert!(part_matches(&either, &p));
        // AND：组内全部成立才行
        let both = vec![vec![
            ("north".to_string(), vec!["true".to_string()]),
            ("south".to_string(), vec!["true".to_string()]),
        ]];
        assert!(!part_matches(&both, &p));
    }

    /// 活塞：推头画在模型 north 面，`facing=up` 是 x:270。
    #[test]
    fn x_rotation_anchored_by_piston() {
        let mut f = [crate::NO_TILE; 6];
        f[NORTH] = 1; // 推头
        f[UP] = 2;
        f[SOUTH] = 3;
        assert_eq!(faces_x(f, 270)[UP], 1, "x=270 顶面应是推头");
        assert_eq!(faces_x(f, 90)[DOWN], 1, "x=90 底面应是推头");
        assert_eq!(faces_x(f, 0)[NORTH], 1);
    }

    /// 梯子：模型画在 +Z（南）面，`facing=east` 是 y:90，朝东的梯子挂在西墙。
    #[test]
    fn y_rotation_anchored_by_ladder() {
        let e = ElementDef { b: [0, 0, 15, 16, 16, 16], f: [crate::NO_TILE; 6], rot: [0; 6], uv: default_uv([0, 0, 15, 16, 16, 16]) };
        let r = transform(&e, 0, 90);
        assert_eq!([r.bbox[0], r.bbox[3]], [0, 1], "朝东的梯子应贴在西侧 x=0..1");
        let mut f = [crate::NO_TILE; 6];
        f[SOUTH] = 7;
        assert_eq!(faces_y(f, 90)[WEST], 7, "模型南面应转到世界西面");
    }

    #[test]
    fn rotation_keeps_full_cube_full() {
        let e = ElementDef { b: [0, 0, 0, 16, 16, 16], f: [0, 1, 2, 3, 4, 5], rot: [0; 6], uv: default_uv([0, 0, 0, 16, 16, 16]) };
        for x in [0, 90, 180, 270] {
            for y in [0, 90, 180, 270] {
                assert_eq!(transform(&e, x, y).bbox, [0, 0, 0, 16, 16, 16], "x={x} y={y}");
            }
        }
    }

    #[test]
    fn rotation_normalizes_reversed_bounds() {
        // 活板门在下半格，绕 x 转 180 度应落到上半格且 from<to
        let e = ElementDef { b: [0, 0, 0, 16, 3, 16], f: [crate::NO_TILE; 6], rot: [0; 6], uv: default_uv([0, 0, 0, 16, 3, 16]) };
        let r = transform(&e, 180, 0);
        assert_eq!(r.bbox, [0, 13, 0, 16, 16, 16]);
        assert!(r.bbox[1] < r.bbox[4]);
    }

    #[test]
    fn face_rotation_is_a_permutation() {
        // 旋转只能重排面，不能凭空产生或丢失
        let f = [10, 11, 12, 13, 14, 15];
        for x in [0, 90, 180, 270] {
            for y in [0, 90, 180, 270] {
                let mut got = faces_y(faces_x(f, x), y);
                got.sort_unstable();
                assert_eq!(got, [10, 11, 12, 13, 14, 15], "x={x} y={y}");
            }
        }
    }

    /// uv 必须跟着面一起旋转，否则活板门转到上半格后贴图区域会错位。
    #[test]
    fn uv_follows_face_rotation() {
        let mut uv = [[0u8; 4]; 6];
        uv[NORTH] = [1, 2, 3, 4];
        let e = ElementDef { b: [0, 0, 0, 16, 16, 16], f: [crate::NO_TILE; 6], uv, rot: [0; 6] };
        // x=270 把模型 north 搬到世界 up
        assert_eq!(transform(&e, 270, 0).uv[UP], [1, 2, 3, 4]);
        // y=90 把模型 north 搬到世界 east
        assert_eq!(transform(&e, 0, 90).uv[EAST], [1, 2, 3, 4]);
    }

    #[test]
    fn default_uv_matches_block_local_coords() {
        // 下半台阶：侧面只取贴图下半部分
        let uv = default_uv([0, 0, 0, 16, 8, 16]);
        assert_eq!(uv[SOUTH], [0, 8, 16, 16], "下半台阶侧面应取 v=8..16");
        assert_eq!(uv[UP], [0, 0, 16, 16], "顶面覆盖整张");
        // 完整方块每个面都是整张
        let uv = default_uv([0, 0, 0, 16, 16, 16]);
        for f in [UP, DOWN, NORTH, SOUTH, EAST, WEST] {
            let r = uv[f];
            assert_eq!((r[2] - r[0], r[3] - r[1]), (16, 16), "面 {f} 应覆盖整张贴图");
        }
    }

    /// 绕 Y 旋转必须把顶面的贴图也转起来，否则南北向和东西向的红石线一模一样。
    #[test]
    fn y_rotation_spins_top_face_texture() {
        let e = ElementDef {
            b: [0, 0, 0, 16, 1, 16],
            f: [7; 6],
            uv: default_uv([0, 0, 0, 16, 1, 16]),
            rot: [0; 6],
        };
        assert_eq!(transform(&e, 0, 0).rot[UP], 0);
        assert_eq!(transform(&e, 0, 90).rot[UP], 1);
        assert_eq!(transform(&e, 0, 180).rot[UP], 2);
        assert_eq!(transform(&e, 0, 270).rot[UP], 3);
        // 底面反向转
        assert_eq!(transform(&e, 0, 90).rot[DOWN], 3);
    }

    #[test]
    fn face_rotation_carried_through_permutation() {
        let mut rot = [0u8; 6];
        rot[NORTH] = 2;
        let e = ElementDef {
            b: [0, 0, 0, 16, 16, 16],
            f: [crate::NO_TILE; 6],
            uv: default_uv([0, 0, 0, 16, 16, 16]),
            rot,
        };
        // x=270 把模型 north 搬到世界 up，旋转要跟着走
        assert_eq!(transform(&e, 270, 0).rot[UP], 2);
    }

    /// 关键回归：模型没声明的面**不画**，绝不能拿同 element 上别的面顶上。
    ///
    /// 以前这里是 `fill_missing_faces`，把有效贴图复制到所有空位。
    /// `redstone_dust_side` 只声明 up/down，四个侧面被顶出来后按包围盒推 uv
    /// 落在贴图最底下一行（红的），在 1/16 厚的薄片上渲染成一个游离的小红点；
    /// 红石火把那 6 片只声明单面的辉光片，六面顶满后糊成一团红。
    #[test]
    fn undeclared_faces_are_not_drawn() {
        // 红石线：只声明 up / down
        let mut f = [NO_FACE; 6];
        f[UP] = 5;
        f[DOWN] = 5;
        assert!(face_drawn(f[UP]) && face_drawn(f[DOWN]));
        for side in [NORTH, SOUTH, EAST, WEST] {
            assert!(!face_drawn(f[side]), "面 {side} 没声明就不该画");
        }
        // 声明了但贴图没解析出来 → NO_TILE，这面还是要画，退回平均色
        assert!(face_drawn(crate::NO_TILE), "NO_TILE 是「没贴图」，不是「不画」");
    }
}
