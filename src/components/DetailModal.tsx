import { useEffect, useState } from 'react'
import { api } from '../api'
import { Viewer3D } from './Viewer3D'
import type { Blueprint, BlueprintDetail } from '../types'
import { formatCount, itemLabel } from '../types'

type Tab = '3d' | 'slice'

/**
 * 蓝图详情。两个视图各有不可替代的用处：
 * - 3D：转着看整体形状，悬停认方块
 * - 切片：生电蓝图从外面看都是个方盒子，内部电路只能逐层看进去
 */
export function DetailModal({ bp, onClose }: { bp: Blueprint; onClose: () => void }) {
  const [tab, setTab] = useState<Tab>('3d')
  const [detail, setDetail] = useState<BlueprintDetail | null>(null)
  const [y, setY] = useState(0)
  const [sliceUrl, setSliceUrl] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    let cancelled = false
    api
      .detail(bp.path)
      .then((d) => {
        if (cancelled) return
        setDetail(d)
        // 默认跳到方块最多的一层，而不是空荡荡的底层
        const best = d.layer_counts.reduce((bi, c, i, a) => (c > a[bi] ? i : bi), 0)
        setY(best)
      })
      .catch((e) => !cancelled && setError(String(e)))
    return () => {
      cancelled = true
    }
  }, [bp.path])

  // 按蓝图的横向尺寸挑格子大小：35×35 的机器该放大到看清单个方块，
  // 500×500 的大工程则要缩到能整屏装下
  const cellPx = Math.max(2, Math.min(24, Math.round(720 / Math.max(bp.size[0], bp.size[2]))))

  useEffect(() => {
    if (!detail || tab !== 'slice') return
    let cancelled = false
    setSliceUrl(null)
    api
      .slice(bp.path, y, cellPx)
      .then((u) => !cancelled && setSliceUrl(u))
      .catch((e) => !cancelled && setError(String(e)))
    return () => {
      cancelled = true
    }
  }, [bp.path, y, detail, cellPx, tab])

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') return onClose()
      if (e.key === '1') return setTab('3d')
      if (e.key === '2') return setTab('slice')
      if (!detail || tab !== 'slice') return
      if (e.key === 'ArrowUp') setY((v) => Math.min(detail.layers - 1, v + 1))
      if (e.key === 'ArrowDown') setY((v) => Math.max(0, v - 1))
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [detail, onClose, tab])

  const maxLayer = detail ? Math.max(...detail.layer_counts, 1) : 1

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/75 p-6"
      onClick={onClose}
    >
      <div
        className="flex h-full w-full max-w-[1500px] flex-col overflow-hidden rounded-xl border border-ink-700 bg-ink-900 shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <header className="flex items-start justify-between gap-4 border-b border-ink-800 px-5 py-3">
          <div className="min-w-0">
            <h2 className="truncate text-base font-medium text-ink-200" title={bp.file_name}>
              {bp.name ?? bp.file_name}
            </h2>
            <p className="mt-0.5 truncate text-xs text-ink-400" title={bp.path}>
              {bp.size[0]}×{bp.size[1]}×{bp.size[2]}
              {bp.author && ` · ${bp.author}`}
              {bp.region_count > 1 && ` · ${bp.region_count} 个 region`}
              {bp.duplicates.length > 0 && ` · ${bp.duplicates.length + 1} 份副本`}
            </p>
          </div>

          <div className="flex shrink-0 items-center gap-2">
            <div className="flex rounded border border-ink-700 p-0.5 text-xs">
              {(
                [
                  ['3d', '3D 预览'],
                  ['slice', '逐层切片'],
                ] as [Tab, string][]
              ).map(([id, label]) => (
                <button
                  key={id}
                  onClick={() => setTab(id)}
                  className={`rounded px-2.5 py-1 transition-colors ${
                    tab === id
                      ? 'bg-accent text-white'
                      : 'text-ink-400 hover:bg-ink-800 hover:text-ink-200'
                  }`}
                >
                  {label}
                </button>
              ))}
            </div>
            <button
              onClick={onClose}
              className="rounded px-2 py-1 text-ink-400 hover:bg-ink-800 hover:text-ink-200"
            >
              ✕
            </button>
          </div>
        </header>

        {error && <div className="px-5 py-2 text-sm text-accent">{error}</div>}

        <div className="flex min-h-0 flex-1">
          <div className="flex min-w-0 flex-1 flex-col bg-ink-950/60">
            {tab === '3d' ? (
              <Viewer3D path={bp.path} size={bp.size} />
            ) : (
              <>
                <div className="scroll-thin flex-1 overflow-auto p-4">
                  {sliceUrl ? (
                    <img src={sliceUrl} alt={`第 ${y} 层`} className="pixelated mx-auto" />
                  ) : (
                    <div className="flex h-full items-center justify-center text-sm text-ink-500">
                      渲染中…
                    </div>
                  )}
                </div>

                {detail && (
                  <div className="border-t border-ink-800 px-5 py-3">
                    <div className="mb-2 flex items-center justify-between text-xs text-ink-400">
                      <span>
                        第 <span className="tnum font-medium text-ink-200">{y}</span> 层 / 共{' '}
                        {detail.layers} 层
                      </span>
                      <span className="tnum">
                        本层 {formatCount(detail.layer_counts[y] ?? 0)} 个方块
                      </span>
                    </div>
                    {/* 每层方块数的直方图，一眼看出哪几层是主体、哪几层是空的 */}
                    <div className="mb-2 flex h-8 items-end gap-px">
                      {detail.layer_counts.map((c, i) => (
                        <button
                          key={i}
                          onClick={() => setY(i)}
                          title={`第 ${i} 层：${formatCount(c)} 个方块`}
                          className={`min-w-[2px] flex-1 rounded-t transition-colors ${
                            i === y ? 'bg-accent' : 'bg-ink-600 hover:bg-ink-500'
                          }`}
                          style={{ height: `${Math.max(6, (c / maxLayer) * 100)}%` }}
                        />
                      ))}
                    </div>
                    <input
                      type="range"
                      min={0}
                      max={Math.max(0, detail.layers - 1)}
                      value={y}
                      onChange={(e) => setY(Number(e.target.value))}
                      className="w-full accent-accent"
                    />
                    <p className="mt-1 text-[11px] text-ink-500">
                      ↑ / ↓ 逐层切换 · 1 / 2 切换视图 · Esc 关闭
                    </p>
                  </div>
                )}
              </>
            )}
          </div>

          <aside className="scroll-thin w-72 shrink-0 overflow-y-auto border-l border-ink-800 p-4">
            <h3 className="mb-2 text-xs font-medium uppercase tracking-wide text-ink-400">
              方块构成
            </h3>
            {detail ? (
              <ul className="space-y-1">
                {detail.top_blocks.map(([name, n]) => (
                  <li key={name} className="flex items-baseline justify-between gap-2 text-xs">
                    <span className="min-w-0 truncate text-ink-300" title={name}>
                      {itemLabel(name)}
                    </span>
                    <span className="tnum shrink-0 text-ink-400">{formatCount(n)}</span>
                  </li>
                ))}
              </ul>
            ) : (
              <p className="text-xs text-ink-500">统计中…</p>
            )}
            {!bp.metadata_trustworthy && (
              <p className="mt-4 rounded border border-amber-900/60 bg-amber-950/40 p-2 text-[11px] leading-relaxed text-amber-300/90">
                这个蓝图的 Metadata 声明值与实算值对不上，说明作者改过元数据。
                这里显示的都是实算值。
              </p>
            )}
          </aside>
        </div>
      </div>
    </div>
  )
}
