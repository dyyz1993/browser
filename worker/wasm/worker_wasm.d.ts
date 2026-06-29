/* tslint:disable */
/* eslint-disable */

/**
 * 提取品牌信息（title/description/og:tags/icon）。
 */
export function extract_branding(html: string, base_url?: string | null): string;

/**
 * 提取高亮文本（mark/strong/em/b）。
 */
export function extract_highlights(html: string, base_url?: string | null): string;

/**
 * 解析 HTML + 提取完整 HTML（序列化 DOM）。
 */
export function extract_html(html: string, base_url?: string | null): string;

/**
 * 提取图片（alt text → URL）。
 */
export function extract_images(html: string, base_url?: string | null): string;

/**
 * 解析 HTML + 提取链接地图。
 */
export function extract_links(html: string, base_url?: string | null): string;

/**
 * 解析 HTML + 提取 markdown。
 */
export function extract_markdown(html: string, base_url?: string | null): string;

/**
 * 解析 HTML + 提取纯文本。
 */
export function extract_text(html: string, base_url?: string | null): string;

/**
 * 提取页面标题。
 */
export function extract_title(html: string): string;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly extract_branding: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly extract_highlights: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly extract_html: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly extract_images: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly extract_links: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly extract_markdown: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly extract_text: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly extract_title: (a: number, b: number, c: number) => void;
    readonly __wbindgen_add_to_stack_pointer: (a: number) => number;
    readonly __wbindgen_export: (a: number, b: number) => number;
    readonly __wbindgen_export2: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_export3: (a: number, b: number, c: number) => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
