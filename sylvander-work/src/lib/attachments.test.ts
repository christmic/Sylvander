import { describe, expect, it } from "vitest";

import { loadSelectedFiles, MAX_COMPOSER_ATTACHMENTS, MAX_LOCAL_ATTACHMENT_BYTES } from "./attachments";

describe("attachment conversion", () => {
  it("maps explicitly selected UTF-8 text to the public attachment contract", async () => {
    const result = await loadSelectedFiles(
      [file(new TextEncoder().encode("hello 世界"), "notes.md", "text/markdown")],
      { allowImages: false, existingCount: 0, startIndex: 7 },
    );
    expect(result).toEqual({
      attachments: [{
        id: "desktop-attachment-7",
        kind: "file",
        name: "notes.md",
        mime_type: "text/markdown",
        content: { encoding: "text", text: "hello 世界" },
        byte_count: 12,
      }],
      errors: [],
    });
  });

  it("sniffs image bytes and gates them on the active model's exact capabilities", async () => {
    const png = file(
      new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1, 2]),
      "image.txt",
      "text/plain",
    );
    const denied = await loadSelectedFiles([png], {
      allowImages: false,
      existingCount: 0,
      startIndex: 1,
    });
    expect(denied.attachments).toEqual([]);
    expect(denied.errors).toEqual(["Active model does not support image attachments"]);

    const accepted = await loadSelectedFiles([png], {
      allowImages: true,
      existingCount: 0,
      startIndex: 2,
    });
    expect(accepted.attachments[0]).toMatchObject({
      id: "desktop-attachment-2",
      kind: "image",
      mime_type: "image/png",
      content: { encoding: "base64", data: "iVBORw0KGgoBAg==" },
      byte_count: 10,
    });
  });

  it("rejects unsupported binary data, oversized files, and attachment overflow", async () => {
    const binary = await loadSelectedFiles(
      [file(new Uint8Array([0xff, 0x00, 0xfe]), "binary.bin", "application/octet-stream")],
      { allowImages: true, existingCount: 0, startIndex: 1 },
    );
    expect(binary.errors[0]).toMatch(/not UTF-8 text, PNG, or JPEG/);

    const oversized = await loadSelectedFiles(
      [file(new Uint8Array(MAX_LOCAL_ATTACHMENT_BYTES + 1), "large.txt", "text/plain")],
      { allowImages: false, existingCount: 0, startIndex: 1 },
    );
    expect(oversized.errors[0]).toMatch(/2 MiB/);

    const overflow = await loadSelectedFiles(
      [file(new Uint8Array([65]), "extra.txt", "text/plain")],
      { allowImages: false, existingCount: MAX_COMPOSER_ATTACHMENTS, startIndex: 1 },
    );
    expect(overflow.attachments).toEqual([]);
    expect(overflow.errors[0]).toMatch(/at most 32/);
  });
});

function file(bytes: Uint8Array, name: string, type: string) {
  const selected = new File([Uint8Array.from(bytes).buffer], name, { type });
  if (typeof selected.arrayBuffer !== "function") {
    Object.defineProperty(selected, "arrayBuffer", {
      value: async () => bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength),
    });
  }
  return selected;
}
