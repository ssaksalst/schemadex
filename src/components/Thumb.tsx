import { useEffect, useRef, useState } from 'react'
import { thumbs } from '../api'

/**
 * 懒加载缩略图。只有滚进视口才去渲染——2000 个蓝图全渲染要几分钟，
 * 而用户一次只看得到十几个。
 */
export function Thumb({ path, id }: { path: string; id: string }) {
  const ref = useRef<HTMLDivElement>(null)
  const [url, setUrl] = useState<string | null>(null)
  const [state, setState] = useState<'idle' | 'loading' | 'error'>('idle')

  useEffect(() => {
    const el = ref.current
    if (!el) return
    let cancelled = false

    const io = new IntersectionObserver(
      (entries) => {
        if (!entries.some((e) => e.isIntersecting)) return
        io.disconnect()
        setState('loading')
        thumbs
          .get(path, id)
          .then((u) => {
            if (!cancelled) {
              setUrl(u)
              setState('idle')
            }
          })
          .catch(() => {
            if (!cancelled) setState('error')
          })
      },
      { rootMargin: '300px' },
    )
    io.observe(el)
    return () => {
      cancelled = true
      io.disconnect()
    }
  }, [path, id])

  return (
    <div
      ref={ref}
      className="flex h-32 items-center justify-center overflow-hidden rounded-t-lg bg-ink-950/60"
    >
      {url ? (
        <img src={url} alt="" className="pixelated max-h-full max-w-full object-contain" />
      ) : state === 'error' ? (
        <span className="text-xs text-ink-500">渲染失败</span>
      ) : (
        <span className="text-xs text-ink-600">{state === 'loading' ? '渲染中…' : ''}</span>
      )}
    </div>
  )
}
