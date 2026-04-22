/**
 * Shared `<IpHostname>` module.
 *
 * See `README.md` in this folder for the exclusivity rules; in short: this
 * module is the single consumer of the `hostname` DTO field, the
 * `/api/hostnames/stream` SSE channel, and `POST /api/hostnames/:ip/refresh`.
 */

export { formatIpWithHostname, hostnameDisplay, tooltipForHostname } from "./format";
export { IpHostname, type IpHostnameProps } from "./IpHostname";
export {
  type HostnameSeedEntry,
  type HostnameValue,
  type IpHostnameContextValue,
  IpHostnameProvider,
} from "./IpHostnameProvider";
export { useIpHostname } from "./useIpHostname";
export { useIpHostnames } from "./useIpHostnames";
export { useRefreshHostname } from "./useRefreshHostname";
export { useSeedHostnamesOnResponse } from "./useSeedHostnamesOnResponse";
