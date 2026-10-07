import { logMsg, media } from "./media";

export type CatalogSong = {
  id: string;
  title?: string;
  artists?: string;
  album?: string;
  durationMs?: number;
  off?: number;
  vip?: number;
  fee?: number;
  pl?: number;
  plLevel?: string;
};

export type CatalogPage = {
  state: "loading" | "ready" | "failed";
  name: string;
  total: number;
  offset: number;
  songs: CatalogSong[];
};

export type CatalogPlaylist = {
  id: string;
  name: string;
  count: number;
};

export type CatalogMenu = {
  state: "loading" | "ready" | "failed";
  playlists?: CatalogPlaylist[];
  /** Compatibility with the native account menu payload. */
  list?: CatalogPlaylist[];
  dailyCount?: number;
  charts?: CatalogPlaylist[];
};

const parse = <T,>(raw: string, fallback: T): T => {
  try {
    return JSON.parse(raw) as T;
  } catch {
    return fallback;
  }
};

/** Native worker version; this call does no file IO and no JSON parsing. */
export const catalogVersion = (): string => media()?.netCatalogVersion?.() || "0";

/** Menu summaries contain only names and counts, never song arrays. */
export const catalogMenu = (kind: "discover" | "charts" | "account"): CatalogMenu => {
  const raw = media()?.netCatalogMenu?.(kind) || "{\"state\":\"loading\"}";
  if (kind === "account") {
    /* This is deliberately the exact bridge return: it tells us whether the
     * native catalog returned list/playlists, loading, or malformed JSON. */
    logMsg(`ui: account_menu_return bytes=${raw.length} raw=${raw}`);
  }
  return parse(raw, { state: "loading" });
};

/** Fetch at most eight visible songs from a native-parsed document. */
export const catalogPage = (
  file: string,
  offset: number,
  limit = 6,
): CatalogPage => {
  const raw =
    media()?.netCatalogPage?.(file, String(offset), String(limit)) ||
    "{\"state\":\"loading\",\"name\":\"\",\"total\":0,\"songs\":[]}";
  const page = parse<CatalogPage>(
    raw,
    { state: "loading", name: "", total: 0, offset, songs: [] },
  );
  if (file.startsWith("playlist_") && (page.state !== "ready" || offset === 0)) {
    const ids = page.songs.map((song) => song.id).filter(Boolean).join(",");
    logMsg(
      `ui: account_page_return file=${file} offset=${offset} state=${page.state} ` +
        `name=${page.name} total=${page.total} songs=${page.songs.length} ids=${ids} raw_bytes=${raw.length}` +
        (page.state === "ready" ? "" : ` raw=${raw}`),
    );
  }
  return { ...page, offset };
};

/** Queue navigation needs IDs only; metadata stays in native snapshots. */
export const catalogIds = (file: string): string[] =>
  parse<string[]>(media()?.netCatalogIds?.(file) || "[]", []);
