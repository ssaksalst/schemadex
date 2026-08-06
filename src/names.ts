import { api } from './api'

/**
 * 方块与物品的中文名。
 *
 * 译名取自游戏自己的 `zh_cn.json`（2600+ 条），整个会话只取一次。
 * 注意这份文件**不在 jar 里**——启动器把它单独存在 assets 仓库，
 * 由后端顺着 jar 路径去捞，捞不到时这里就是空表，界面自动退回英文。
 */
let cache: Record<string, string> | null = null
let pending: Promise<Record<string, string>> | null = null

export function loadNames(): Promise<Record<string, string>> {
  if (cache) return Promise.resolve(cache)
  if (!pending) {
    pending = api
      .names()
      .then((n) => {
        cache = n
        return n
      })
      .catch(() => {
        cache = {}
        return cache
      })
  }
  return pending
}

/** 同步查询；没加载完或没有译名时返回 null。 */
export function chineseName(id: string): string | null {
  return cache?.[id] ?? null
}

/** `minecraft:sticky_piston` → `粘性活塞`，没有译名时退回 `sticky piston` */
export function label(id: string): string {
  return chineseName(id) ?? id.replace(/^minecraft:/, '').replace(/_/g, ' ')
}
