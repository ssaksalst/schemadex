//! 方块状态 → 所需物品 的映射，以及材料清单汇总。
//!
//! 这是整个工具最容易出错、也最不能出错的地方：数字错了就没人敢照着备货。
//! Litematica 在游戏里能直接调 `Block.getCloneItemStack()` 拿到正确物品，
//! 离线没有这个能力，只能靠这张手工表。表覆盖三类差异：
//!
//! 1. **无对应物品**（活塞臂、火、传送门方块）—— 不计
//! 2. **方块名 ≠ 物品名**（`redstone_wire`→`redstone`、`wheat`→`wheat_seeds`）
//! 3. **一个方块要多个物品**（双台阶 ×2、蜡烛 ×n、海泡菜 ×n）
//!
//! 其余默认「方块名即物品名」，这对绝大多数方块成立。

use std::collections::{BTreeMap, BTreeSet};

use crate::schematic::{BlockState, Schematic};

/// 一个方块状态需要的物品。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    pub item: String,
    /// 该方块需要几个此物品
    pub per_block: u32,
    /// 映射是否精确。`false` 表示做了近似（如蜡烛蛋糕只算蛋糕），
    /// UI 应当把这些标出来，让用户知道哪几行数字要自己复核。
    pub exact: bool,
}

impl Requirement {
    fn exact(item: &str) -> Option<Self> {
        Some(Self { item: format!("minecraft:{item}"), per_block: 1, exact: true })
    }
    fn approx(item: &str) -> Option<Self> {
        Some(Self { item: format!("minecraft:{item}"), per_block: 1, exact: false })
    }
    fn same(bs: &BlockState, n: u32) -> Option<Self> {
        Some(Self { item: bs.name.clone(), per_block: n, exact: true })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MaterialOptions {
    /// 是否把流体方块算成桶。Litematica 会算，但含水方块（waterlogged）
    /// 很容易让数字虚高，所以默认关掉。
    pub count_fluids: bool,
    /// 是否统计蓝图里容器内已有的物品（分类系统的样板物品、备货等）
    pub count_container_items: bool,
}

impl Default for MaterialOptions {
    fn default() -> Self {
        Self { count_fluids: false, count_container_items: true }
    }
}

/// 去掉 `minecraft:` 前缀；非 minecraft 命名空间返回 None。
fn vanilla(name: &str) -> Option<&str> {
    name.strip_prefix("minecraft:")
        .or_else(|| if name.contains(':') { None } else { Some(name) })
}

/// 方块状态 → 所需物品。返回 `None` 表示不需要任何材料。
pub fn block_to_item(bs: &BlockState, opts: &MaterialOptions) -> Option<Requirement> {
    let Some(id) = vanilla(&bs.name) else {
        // 模组方块：只能原样计数
        return Some(Requirement { item: bs.name.clone(), per_block: 1, exact: false });
    };

    // --- 1. 空气与技术方块 ---
    match id {
        "air" | "cave_air" | "void_air" => return None,
        // 活塞推出时的臂和位移中的方块，本体已单独计数
        "piston_head" | "moving_piston" => return None,
        "fire" | "soul_fire" => return None,
        "nether_portal" | "end_portal" | "end_gateway" => return None,
        "bubble_column" => return None,
        // 紫颂植物的茎无法放置，只有花有物品
        "chorus_plant" => return None,
        _ => {}
    }

    // --- 2. 双方块的上半 / 床头：只算一次 ---
    // 注意楼梯和活板门的 half 是 top/bottom，不是 upper，不会误伤
    if bs.prop("half") == Some("upper") {
        return None;
    }
    if bs.prop("part") == Some("head") {
        return None;
    }

    // --- 3. 流体 ---
    match id {
        "water" => {
            return if opts.count_fluids { Requirement::exact("water_bucket") } else { None }
        }
        "lava" => {
            return if opts.count_fluids { Requirement::exact("lava_bucket") } else { None }
        }
        _ => {}
    }

    // --- 4. 需要按属性乘数量的方块 ---
    // 双层台阶要两个台阶
    if id.ends_with("_slab") && bs.prop("type") == Some("double") {
        return Requirement::same(bs, 2);
    }
    // 蜡烛 1~4 根
    if id == "candle" || id.ends_with("_candle") {
        let n = bs.prop("candles").and_then(|v| v.parse::<u32>().ok()).unwrap_or(1);
        return Requirement::same(bs, n.clamp(1, 4));
    }
    if id == "sea_pickle" {
        let n = bs.prop("pickles").and_then(|v| v.parse::<u32>().ok()).unwrap_or(1);
        return Requirement::same(bs, n.clamp(1, 4));
    }
    if id == "turtle_egg" {
        let n = bs.prop("eggs").and_then(|v| v.parse::<u32>().ok()).unwrap_or(1);
        return Requirement::same(bs, n.clamp(1, 4));
    }
    if id == "snow" {
        // 雪层：放 n 层要 n 个雪层物品
        let n = bs.prop("layers").and_then(|v| v.parse::<u32>().ok()).unwrap_or(1);
        return Requirement::same(bs, n.clamp(1, 8));
    }

    // --- 5. 方块名 ≠ 物品名 ---
    if let Some(item) = remap_exact(id) {
        return Requirement::exact(item);
    }

    // 墙上变体 → 地面/常规变体
    if let Some(item) = remap_wall_variant(id) {
        return Requirement::exact(&item);
    }

    // 近似映射：数字不保证精确，UI 会标出来
    if let Some(item) = remap_approximate(id) {
        return Requirement::approx(item);
    }

    // --- 6. 默认：方块名即物品名 ---
    Requirement::same(bs, 1)
}

/// 一对一的精确改名。
fn remap_exact(id: &str) -> Option<&'static str> {
    Some(match id {
        // 红石
        "redstone_wire" => "redstone",
        "tripwire" => "string",
        // 作物
        "wheat" => "wheat_seeds",
        "carrots" => "carrot",
        "potatoes" => "potato",
        "beetroots" => "beetroot_seeds",
        "melon_stem" | "attached_melon_stem" => "melon_seeds",
        "pumpkin_stem" | "attached_pumpkin_stem" => "pumpkin_seeds",
        "cocoa" => "cocoa_beans",
        "sweet_berry_bush" => "sweet_berries",
        "torchflower_crop" => "torchflower_seeds",
        "pitcher_crop" => "pitcher_pod",
        // 植物的「茎」段：只有顶端有物品
        "kelp_plant" => "kelp",
        "twisting_vines_plant" => "twisting_vines",
        "weeping_vines_plant" => "weeping_vines",
        "cave_vines" | "cave_vines_plant" => "glow_berries",
        "big_dripleaf_stem" => "big_dripleaf",
        "bamboo_sapling" => "bamboo",
        // 由工具改造而成的方块，物品是原料
        "farmland" | "dirt_path" => "dirt",
        // 装了东西的炼药锅
        "water_cauldron" | "lava_cauldron" | "powder_snow_cauldron" => "cauldron",
        // 未点燃 / 已点燃的变体
        "redstone_wall_torch" => "redstone_torch",
        "soul_wall_torch" => "soul_torch",
        "wall_torch" => "torch",
        _ => return None,
    })
}

/// `*_wall_xxx` → `*_xxx` 这一类规则化改名。
fn remap_wall_variant(id: &str) -> Option<String> {
    for (suffix, replacement) in [
        ("_wall_hanging_sign", "_hanging_sign"),
        ("_wall_sign", "_sign"),
        ("_wall_banner", "_banner"),
        ("_wall_fan", "_fan"),
        ("_wall_skull", "_skull"),
        ("_wall_head", "_head"),
    ] {
        if let Some(stem) = id.strip_suffix(suffix) {
            return Some(format!("{stem}{replacement}"));
        }
    }
    None
}

/// 已知不精确的映射。宁可标出来让人复核，也不要给个看起来精确的错数字。
fn remap_approximate(id: &str) -> Option<&'static str> {
    Some(match id {
        // 蜡烛蛋糕实际是蛋糕 + 1 根蜡烛，这里只算蛋糕
        _ if id.ends_with("_candle_cake") => "cake",
        "candle_cake" => "cake",
        _ => return None,
    })
}

/// 一份材料清单。
#[derive(Debug, Clone, Default)]
pub struct MaterialList {
    /// 建造所需物品 → 数量
    pub blocks: BTreeMap<String, u64>,
    /// 蓝图里容器内已有的物品 → 数量（分类系统的样板物品等）
    pub container_items: BTreeMap<String, u64>,
    /// 用到了近似映射的物品，UI 应提示用户复核
    pub inexact: BTreeSet<String>,
    /// 实算的非空气方块总数
    pub total_blocks: u64,
    /// 实算总体积
    pub total_volume: u64,
}

impl MaterialList {
    pub fn of(schematic: &Schematic, opts: &MaterialOptions) -> Self {
        let mut out = Self { total_volume: schematic.total_volume(), ..Default::default() };

        for region in &schematic.regions {
            // 先按调色板算好每个 palette 项的需求，再乘直方图计数。
            // 这样 5 亿方块的蓝图也只做 5 亿次加法，不做 5 亿次字符串操作。
            let reqs: Vec<Option<Requirement>> =
                region.palette.iter().map(|bs| block_to_item(bs, opts)).collect();
            let non_air: Vec<bool> = region.palette.iter().map(|bs| !bs.is_air()).collect();

            let hist = region.palette_histogram();
            for (i, &count) in hist.iter().enumerate() {
                if count == 0 {
                    continue;
                }
                if non_air.get(i).copied().unwrap_or(false) {
                    out.total_blocks += count;
                }
                if let Some(Some(req)) = reqs.get(i) {
                    *out.blocks.entry(req.item.clone()).or_insert(0) +=
                        count * req.per_block as u64;
                    if !req.exact {
                        out.inexact.insert(req.item.clone());
                    }
                }
            }

            if opts.count_container_items {
                for te in &region.tile_entities {
                    for stack in &te.items {
                        *out.container_items.entry(stack.id.clone()).or_insert(0) +=
                            stack.count.max(0) as u64;
                    }
                }
            }
        }
        out
    }

    /// 合并另一份清单（跨蓝图汇总用）。
    pub fn merge(&mut self, other: &MaterialList) {
        for (k, v) in &other.blocks {
            *self.blocks.entry(k.clone()).or_insert(0) += v;
        }
        for (k, v) in &other.container_items {
            *self.container_items.entry(k.clone()).or_insert(0) += v;
        }
        self.inexact.extend(other.inexact.iter().cloned());
        self.total_blocks += other.total_blocks;
        self.total_volume += other.total_volume;
    }

    /// 按数量降序的建筑材料。
    pub fn sorted_blocks(&self) -> Vec<(&str, u64)> {
        let mut v: Vec<(&str, u64)> =
            self.blocks.iter().map(|(k, n)| (k.as_str(), *n)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        v
    }
}

/// 把个数换算成「盒+组+个」。生电备货就是按这个单位说话的。
/// 一组 64，一盒 27 组 = 1728。
pub fn to_stacks(count: u64, stack_size: u64) -> (u64, u64, u64) {
    let per_box = stack_size * 27;
    (count / per_box, (count % per_box) / stack_size, count % stack_size)
}

/// 常见的非 64 堆叠上限。默认 64。
pub fn stack_size(item: &str) -> u64 {
    let Some(id) = vanilla(item) else { return 64 };
    if id.ends_with("_bucket")
        || id.ends_with("_boat")
        || id.ends_with("_chest_boat")
        || id.ends_with("_bed")
        || id.ends_with("_shulker_box")
        || id.ends_with("_banner")
    {
        return 1;
    }
    match id {
        "bucket" | "water_bucket" | "lava_bucket" | "milk_bucket" | "powder_snow_bucket"
        | "shulker_box" | "cake" | "saddle" | "minecart" | "chest_minecart"
        | "hopper_minecart" | "furnace_minecart" | "tnt_minecart" | "beacon" => 1,
        "ender_pearl" | "snowball" | "egg" | "sign" | "honey_bottle" | "armor_stand" => 16,
        _ => 64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn bs(name: &str, props: &[(&str, &str)]) -> BlockState {
        BlockState {
            name: format!("minecraft:{name}"),
            properties: props
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>(),
        }
    }
    fn item_of(name: &str, props: &[(&str, &str)]) -> Option<(String, u32)> {
        block_to_item(&bs(name, props), &MaterialOptions::default())
            .map(|r| (r.item, r.per_block))
    }

    #[test]
    fn air_and_technical_blocks_cost_nothing() {
        for n in ["air", "cave_air", "void_air", "piston_head", "moving_piston", "fire",
                  "nether_portal", "bubble_column", "chorus_plant"] {
            assert_eq!(item_of(n, &[]), None, "{n} 不应计入材料");
        }
    }

    #[test]
    fn redstone_wire_is_redstone_dust() {
        assert_eq!(
            item_of("redstone_wire", &[("power", "0")]),
            Some(("minecraft:redstone".into(), 1))
        );
    }

    #[test]
    fn crops_map_to_seeds_or_produce() {
        assert_eq!(item_of("wheat", &[("age", "7")]).unwrap().0, "minecraft:wheat_seeds");
        assert_eq!(item_of("carrots", &[]).unwrap().0, "minecraft:carrot");
        assert_eq!(item_of("beetroots", &[]).unwrap().0, "minecraft:beetroot_seeds");
        assert_eq!(item_of("cocoa", &[]).unwrap().0, "minecraft:cocoa_beans");
        assert_eq!(item_of("melon_stem", &[]).unwrap().0, "minecraft:melon_seeds");
    }

    #[test]
    fn double_blocks_counted_once() {
        // 门的上半、床头不重复计
        assert_eq!(item_of("oak_door", &[("half", "upper")]), None);
        assert_eq!(item_of("oak_door", &[("half", "lower")]).unwrap().0, "minecraft:oak_door");
        assert_eq!(item_of("red_bed", &[("part", "head")]), None);
        assert_eq!(item_of("red_bed", &[("part", "foot")]).unwrap().0, "minecraft:red_bed");
        assert_eq!(item_of("tall_grass", &[("half", "upper")]), None);
    }

    #[test]
    fn stairs_and_trapdoors_not_mistaken_for_double_blocks() {
        // 它们的 half 是 top/bottom，不是 upper——不能被上半规则误伤
        assert_eq!(
            item_of("oak_stairs", &[("half", "top")]).unwrap().0,
            "minecraft:oak_stairs"
        );
        assert_eq!(
            item_of("oak_trapdoor", &[("half", "top")]).unwrap().0,
            "minecraft:oak_trapdoor"
        );
    }

    #[test]
    fn double_slab_needs_two() {
        assert_eq!(item_of("oak_slab", &[("type", "double")]), Some(("minecraft:oak_slab".into(), 2)));
        assert_eq!(item_of("oak_slab", &[("type", "top")]), Some(("minecraft:oak_slab".into(), 1)));
    }

    #[test]
    fn multi_count_properties() {
        assert_eq!(item_of("candle", &[("candles", "3")]).unwrap().1, 3);
        assert_eq!(item_of("sea_pickle", &[("pickles", "4")]).unwrap().1, 4);
        assert_eq!(item_of("turtle_egg", &[("eggs", "2")]).unwrap().1, 2);
        assert_eq!(item_of("snow", &[("layers", "5")]).unwrap().1, 5);
    }

    #[test]
    fn wall_variants_map_to_base_item() {
        assert_eq!(item_of("wall_torch", &[]).unwrap().0, "minecraft:torch");
        assert_eq!(item_of("redstone_wall_torch", &[]).unwrap().0, "minecraft:redstone_torch");
        assert_eq!(item_of("oak_wall_sign", &[]).unwrap().0, "minecraft:oak_sign");
        assert_eq!(item_of("oak_wall_hanging_sign", &[]).unwrap().0, "minecraft:oak_hanging_sign");
        assert_eq!(item_of("white_wall_banner", &[]).unwrap().0, "minecraft:white_banner");
        assert_eq!(item_of("skeleton_wall_skull", &[]).unwrap().0, "minecraft:skeleton_skull");
        assert_eq!(item_of("zombie_wall_head", &[]).unwrap().0, "minecraft:zombie_head");
        assert_eq!(item_of("tube_coral_wall_fan", &[]).unwrap().0, "minecraft:tube_coral_fan");
    }

    #[test]
    fn ordinary_blocks_pass_through() {
        for n in ["stone", "observer", "hopper", "sticky_piston", "note_block", "obsidian",
                  "packed_ice", "smooth_stone", "repeater", "comparator"] {
            assert_eq!(item_of(n, &[]).unwrap().0, format!("minecraft:{n}"), "{n}");
        }
    }

    #[test]
    fn fluids_off_by_default() {
        assert_eq!(item_of("water", &[]), None);
        let on = MaterialOptions { count_fluids: true, ..Default::default() };
        assert_eq!(
            block_to_item(&bs("water", &[]), &on).unwrap().item,
            "minecraft:water_bucket"
        );
    }

    #[test]
    fn approximate_mappings_are_flagged() {
        let r = block_to_item(&bs("white_candle_cake", &[]), &MaterialOptions::default()).unwrap();
        assert_eq!(r.item, "minecraft:cake");
        assert!(!r.exact, "蜡烛蛋糕是近似映射，必须标记");
    }

    #[test]
    fn stack_math() {
        assert_eq!(to_stacks(0, 64), (0, 0, 0));
        assert_eq!(to_stacks(64, 64), (0, 1, 0));
        assert_eq!(to_stacks(1728, 64), (1, 0, 0));
        assert_eq!(to_stacks(1729, 64), (1, 0, 1));
        assert_eq!(to_stacks(1728 * 2 + 64 * 3 + 5, 64), (2, 3, 5));
    }

    #[test]
    fn stack_sizes() {
        assert_eq!(stack_size("minecraft:stone"), 64);
        assert_eq!(stack_size("minecraft:water_bucket"), 1);
        assert_eq!(stack_size("minecraft:red_bed"), 1);
        assert_eq!(stack_size("minecraft:white_shulker_box"), 1);
        assert_eq!(stack_size("minecraft:ender_pearl"), 16);
    }
}
