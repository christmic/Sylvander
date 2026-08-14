/**
 * Browser-selected attachment conversion for the public Runtime contract.
 *
 * The WebView reads only files explicitly granted through an `<input>`
 * selection. It never receives a filesystem path or Tauri filesystem access.
 */

import type { RuntimeMessageAttachment } from "./gateway";

export const MAX_COMPOSER_ATTACHMENTS = 32;
export const MAX_LOCAL_ATTACHMENT_BYTES = 2 * 1024 * 1024;

export interface AttachmentLoadResult {
  attachments: RuntimeMessageAttachment[];
  errors: string[];
}

export async function loadSelectedFiles(
  files: File[],
  options: { allowImages: boolean; existingCount: number; startIndex: number },
): Promise<AttachmentLoadResult> {
  const available = Math.max(0, MAX_COMPOSER_ATTACHMENTS - options.existingCount);
  const accepted = files.slice(0, available);
  const errors = files.length > available
    ? [`Composer supports at most ${MAX_COMPOSER_ATTACHMENTS} attachments`]
    : [];
  const attachments: RuntimeMessageAttachment[] = [];
  for (const [offset, file] of accepted.entries()) {
    try {
      attachments.push(await loadFile(file, options.allowImages, options.startIndex + offset));
    } catch (error) {
      errors.push(error instanceof Error ? error.message : "Attachment could not be read");
    }
  }
  return { attachments, errors };
}

async function loadFile(file: File, allowImages: boolean, index: number) {
  if (!file.name || file.name.trim() !== file.name || [...file.name].some((character) => isControl(character))) {
    throw new Error("Attachment name is invalid");
  }
  if (file.size > MAX_LOCAL_ATTACHMENT_BYTES) {
    throw new Error(`${file.name} exceeds the 2 MiB local attachment limit`);
  }
  const bytes = new Uint8Array(await file.arrayBuffer());
  const imageMime = sniffImageMime(bytes);
  if (imageMime) {
    if (!allowImages) throw new Error("Active model does not support image attachments");
    return {
      id: `desktop-attachment-${index}`,
      kind: "image" as const,
      name: file.name,
      mime_type: imageMime,
      content: { encoding: "base64" as const, data: encodeBase64(bytes) },
      byte_count: bytes.byteLength,
    };
  }
  let text: string;
  try {
    text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    throw new Error(`${file.name} is not UTF-8 text, PNG, or JPEG`);
  }
  return {
    id: `desktop-attachment-${index}`,
    kind: "file" as const,
    name: file.name,
    mime_type: textMime(file),
    content: { encoding: "text" as const, text },
    byte_count: bytes.byteLength,
  };
}

function sniffImageMime(bytes: Uint8Array) {
  const png = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
  if (bytes.length >= png.length && png.every((byte, index) => bytes[index] === byte)) {
    return "image/png" as const;
  }
  if (bytes.length >= 3 && bytes[0] === 0xff && bytes[1] === 0xd8 && bytes[2] === 0xff) {
    return "image/jpeg" as const;
  }
  return undefined;
}

function textMime(file: File) {
  if (file.type.startsWith("text/")) return file.type;
  const extension = file.name.split(".").pop()?.toLowerCase();
  if (extension === "json") return "application/json";
  if (extension === "md") return "text/markdown";
  if (extension === "diff" || extension === "patch") return "text/x-diff";
  return "text/plain";
}

function encodeBase64(bytes: Uint8Array) {
  let binary = "";
  const chunkSize = 32 * 1024;
  for (let offset = 0; offset < bytes.length; offset += chunkSize) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + chunkSize));
  }
  return btoa(binary);
}

function isControl(character: string) {
  const code = character.codePointAt(0) ?? 0;
  return code <= 0x1f || code === 0x7f;
}
