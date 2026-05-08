// Thin fetch wrapper. Sends session cookie on every request and throws
// ApiError so callers can branch on status codes.

export class ApiError extends Error {
  status: number
  code?: string
  constructor(status: number, message: string, code?: string) {
    super(message)
    this.status = status
    this.code = code
  }
}

async function parseError(res: globalThis.Response): Promise<ApiError> {
  let body: any = null
  try { body = await res.json() } catch { /* not JSON */ }
  const message = body?.error?.message || res.statusText || `HTTP ${res.status}`
  return new ApiError(res.status, message, body?.error?.code)
}

export async function request<T>(path: string, init: RequestInit & { json?: any } = {}): Promise<T> {
  const headers = new Headers(init.headers)
  let body = init.body as BodyInit | undefined
  if (init.json !== undefined) {
    headers.set('Content-Type', 'application/json')
    body = JSON.stringify(init.json)
  }
  const res = await fetch(path, { ...init, body, headers, credentials: 'include' })
  if (!res.ok) throw await parseError(res)
  if (res.status === 204) return undefined as unknown as T
  const text = await res.text()
  if (!text) return undefined as unknown as T
  try { return JSON.parse(text) as T } catch { return text as unknown as T }
}

export const api = {
  get:   <T,>(path: string)           => request<T>(path, { method: 'GET' }),
  post:  <T,>(path: string, json?: any) => request<T>(path, { method: 'POST', json }),
  patch: <T,>(path: string, json?: any) => request<T>(path, { method: 'PATCH', json }),
  del:   <T,>(path: string)           => request<T>(path, { method: 'DELETE' }),
}
