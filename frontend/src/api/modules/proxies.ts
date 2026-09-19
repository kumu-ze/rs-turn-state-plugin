import request from '../request'

export interface OutboundProxyRecord { id: string, name: string, endpoint: string, hasAuthentication: boolean }
export function getProxies(input: { page: number, pageSize: number }) {
  return request<{ items: OutboundProxyRecord[], page: { totalPages: number } }>({ url: '/api/admin/proxies', data: input })
}
