import type { RequestOptions } from '../request'
import request from '../request'

export type TicketMode = 'off' | 'manual' | 'auto'
export interface TicketPolicy { mode: TicketMode, targetLength: number | null }
export interface TicketSettings {
  enabled: boolean
  inject: boolean
  proxyPoolEnabled: boolean
  activityOnly: boolean
  idleSeconds: number
  requireTicket: boolean
  plusProLength: number
  businessLength: number
  defaultLength: number
  models: string[]
  intervalSeconds: number
  manualIntervalSeconds: number
  ttlSeconds: number
  refreshBeforeSeconds: number
  accounts: Record<string, TicketPolicy>
}
export interface TicketResult {
  accountId: string
  model: string
  httpStatus: number
  length: number
  matched: boolean
  checkedAt: number
  message: string
}
export interface TicketAccount {
  id: string
  name: string
  plan: string | null
  eligible: boolean
  targetLength: number
  policy: TicketPolicy
  models: { model: string, ready: boolean, expiresAt: number | null, tokenIssuedAt: number | null, lastRequestedAt: number | null, autoPaused: boolean, blocked: boolean, lastResult: TicketResult | null, busy: boolean, retryAt: number | null, manualRetryAt: number | null, continuous: { intervalSeconds: number, nextProbeAt: number, proxyId: string | null } | null }[]
}
export interface TicketLog {
  id: string
  accountName: string
  trigger: TicketMode
  continuous: boolean
  proxyEndpoint: string
  proxyName: string
  targetLength: number
  startedAt: number
  durationMs: number
  retryAt: number | null
  result: TicketResult
  tokenIssuedAt: number | null
  exitSample: TicketExitSample | null
}
export interface TicketExitSample { ip: string | null, checkedAt: number, message: string }
export interface TicketProxyInput { id?: string, name: string, url?: string, savedProxyId?: string, enabled?: boolean, concurrency?: number }
export interface TicketProxyView { id: string, name: string, endpoint: string, hasAuthentication: boolean, enabled: boolean, concurrency: number, inFlight: number, exitSample: TicketExitSample | null }
export interface TicketPanel { revision: number, settings: TicketSettings, proxyConfigured: boolean, proxyCount: number, proxies: TicketProxyView[], accounts: TicketAccount[], logs: TicketLog[], logLimit: number, workerCheckedAt: number | null }

export function getTicketPanel(options: RequestOptions = {}) {
  return request<TicketPanel>({ url: '/api/admin/tickets', ...options })
}
export function saveTicketSettings(data: unknown, options: RequestOptions = {}) {
  return request<TicketPanel>({ url: '/api/admin/tickets', method: 'POST', data, ...options })
}
export interface TicketProbeInput { accountId: string, model: string, proxyId?: string, revision?: number }
export function probeTicket(data: TicketProbeInput) {
  return request({ url: '/api/admin/tickets/probe', method: 'POST', data })
}
export function continuousTicket(data: TicketProbeInput & { intervalSeconds: number | null }) {
  return request<TicketPanel>({ url: '/api/admin/tickets/continuous', method: 'POST', data })
}
export function clearTicketLogs() {
  return request<TicketPanel>({ url: '/api/admin/tickets/logs/clear', method: 'POST' })
}
export function sampleTicketExit(data: { proxyId: string, revision: number }) {
  return request({ url: '/api/admin/tickets/proxy-exit', method: 'POST', data })
}
