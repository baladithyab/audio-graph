/**
 * Browser-side download helpers for exporting transcripts and knowledge graphs.
 *
 * These functions run entirely in the webview (no Tauri FS plugin required).
 * They synthesize a temporary `<a download>` element, trigger a click, and
 * clean up the object URL afterwards.
 */

import type { TranscriptSegment } from "../types";
import { formatTime } from "./format";

/**
 * Trigger a browser download for `content` as a file named `filename`.
 *
 * @param content    The file contents (string).
 * @param filename   The suggested download filename.
 * @param mimeType   MIME type. Defaults to `application/json`.
 */
export function downloadAsFile(
    content: string,
    filename: string,
    mimeType: string = "application/json",
): void {
    const blob = new Blob([content], { type: mimeType });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = filename;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
}

/**
 * Convert transcript segments to a plain-text representation with one line
 * per segment in the form `[MM:SS] Speaker: text`.
 */
export function transcriptToTxt(segments: TranscriptSegment[]): string {
    return segments
        .map((s) => {
            const time = formatTime(s.start_time);
            const speaker = s.speaker_label ?? "Unknown";
            return `[${time}] ${speaker}: ${s.text}`;
        })
        .join("\n");
}

/**
 * Build a `YYYYMMDD-HHMMSS` timestamp suitable for use in export filenames.
 * Uses local time; safe for filename characters on all major OSes.
 */
export function filenameTimestamp(date: Date = new Date()): string {
    const pad = (n: number) => n.toString().padStart(2, "0");
    return (
        `${date.getFullYear()}${pad(date.getMonth() + 1)}${pad(date.getDate())}` +
        `-${pad(date.getHours())}${pad(date.getMinutes())}${pad(date.getSeconds())}`
    );
}
