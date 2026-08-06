//! Minecraft 方块材质与模型。
//!
//! 从客户端 jar 提取三样东西：
//! - **模型库** —— 每个模型是若干带六面贴图的长方体
//! - **状态规则** —— blockstate 的 variant / multipart，运行时按真实方块状态解析
//! - **图集** —— 所有 16×16 贴图拼成一张大图，给等距渲染和 WebGL 用
//!
//! 外加 `lang/zh_cn.json` 里的中文名。
//!
//! 关键取舍是**保留 MC 模型系统的原始结构**而不在提取时压扁。蓝图里的方块
//! 带着 `facing` / `half` / `axis` 等状态，压成「一个方块名 → 一套贴图 + 一个盒子」
//! 会让所有活塞都朝上、所有活板门都在下半格、楼梯只剩一整块、栅栏没有横杆。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

mod extract;
pub mod model;

pub use extract::extract;
pub use model::{BlockDef, ElementDef, ModelDef, PartDef, ResolvedBox, VariantDef};

pub type Rgb = [u8; 3];

/// 图块索引的空值：**这一面要画，但没找到贴图**，退回平均色。
/// 渲染器自己造的占位长方体（降采样、方块实体）也用它。
pub const NO_TILE: u16 = u16::MAX;

/// **模型压根没声明这一面**——不是没贴图，是这面根本不该画。
///
/// MC 对模型里没写的面就是不画。以前我们把它和 `NO_TILE` 混为一谈，
/// 再拿同 element 上任意一个面的贴图去顶（原来的 `fill_missing_faces`），
/// 于是凭空造出了两个 bug：
///
/// - 红石线：`redstone_dust_side` 只声明 up/down，被顶出来的四个侧面
///   按包围盒推 uv 落在贴图最底下那一行（正好是红的），
///   在 1/16 厚的薄片侧面上渲染成一个**游离的小红点**。
/// - 红石火把：1.21.4 的 `template_redstone_torch` 有 6 个零厚度、
///   各自只声明一个面的「辉光片」。六面都顶上贴图后，
///   每片都成了实心小方块，糊成一团红。
///
/// 两个渲染器都必须**跳过**这个值，不能退回平均色——退回平均色只是把
/// 「带纹理的错东西」换成「纯色的错东西」，那个红点还在。
pub const NO_FACE: u16 = u16::MAX - 1;

/// 方块的代表色。切片视图与没有模型时兜底用。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlockColor {
    pub top: Rgb,
    pub side: Rgb,
}

/// 方块材质表。序列化成 JSON，图集 PNG 单独存一个文件
/// （base64 塞进 JSON 会让文件大三分之一，且每次读都要重解码）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockAssets {
    /// 来源 jar 的游戏版本，例如 "1.21.4"
    pub version: String,
    /// "minecraft:stone" → 平均色
    pub colors: BTreeMap<String, BlockColor>,
    /// 去重后的模型库，`BlockDef` 里的下标指向它
    #[serde(default)]
    pub models: Vec<ModelDef>,
    /// "minecraft:stone" → variant / multipart 规则
    #[serde(default)]
    pub blocks: BTreeMap<String, BlockDef>,
    /// 中文名。键是完整 ID（方块与物品都有），取自 `lang/zh_cn.json`
    #[serde(default)]
    pub names: BTreeMap<String, String>,
    /// 单个图块边长（像素）
    #[serde(default)]
    pub tile_size: u32,
    /// 图集每行多少个图块
    #[serde(default)]
    pub tiles_per_row: u32,
    /// 图集里的图块总数
    #[serde(default)]
    pub tile_count: u32,
    /// 解析不出材质的方块，供排查
    pub unresolved: Vec<String>,
}

impl BlockAssets {
    pub fn get(&self, block: &str) -> Option<BlockColor> {
        self.colors.get(block).copied()
    }

    /// 中文名。没有译名时返回 None，调用方自行退回英文 ID。
    pub fn name_of(&self, id: &str) -> Option<&str> {
        self.names.get(id).map(String::as_str)
    }

    /// 按真实方块状态解析出一组带贴图的长方体。
    ///
    /// 这是整套材质系统的入口：蓝图里的方块带着 `facing` / `half` / `axis`
    /// 等状态，只有把它们喂进来，活塞才会朝对方向、活板门才会在对的半格、
    /// 楼梯才有两段、栅栏才长出横杆。
    ///
    /// 返回空表示这个方块没有可渲染的模型（箱子之类的方块实体），
    /// 调用方应退回 [`BlockAssets::get`] 的平均色。
    pub fn resolve(&self, block: &str, props: &BTreeMap<String, String>) -> Vec<ResolvedBox> {
        let mut out = Vec::new();
        let Some(def) = self.blocks.get(block) else { return out };

        let mut push = |m: u32, x: i16, y: i16| {
            let Some(md) = self.models.get(m as usize) else { return };
            for e in &md.e {
                out.push(model::transform(e, x as i64, y as i64));
            }
        };
        match def {
            BlockDef::Variants(list) => {
                // 有序，取第一个匹配的。
                //
                // **一个都匹配不上时必须兜底到第一条**。红石火把的 variant 键是
                // `lit=true` / `lit=false`，只要传进来的状态里没有 `lit`，
                // 两条都不匹配，方块就会整个消失、退化成一个纯色立方体。
                let chosen = list
                    .iter()
                    .find(|v| model::variant_matches(&v.w, props))
                    .or_else(|| list.first());
                if let Some(v) = chosen {
                    push(v.m, v.x, v.y);
                }
            }
            BlockDef::Multipart(parts) => {
                // 所有匹配的部件叠加——栅栏的柱子和横杆就是这么拼出来的
                for p in parts.iter().filter(|p| model::part_matches(&p.w, props)) {
                    push(p.m, p.x, p.y);
                }
            }
        }
        out
    }

    /// 图块索引 → 图集里的左上角像素坐标
    pub fn tile_origin(&self, tile: u16) -> Option<(u32, u32)> {
        if tile == NO_TILE || self.tiles_per_row == 0 {
            return None;
        }
        let t = tile as u32;
        Some((
            (t % self.tiles_per_row) * self.tile_size,
            (t / self.tiles_per_row) * self.tile_size,
        ))
    }
}

/// 提取结果：材质表 + 图集 PNG。
pub struct Extracted {
    pub assets: BlockAssets,
    pub atlas_png: Vec<u8>,
}
