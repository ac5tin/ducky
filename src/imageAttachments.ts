import type { ImagePart } from "./types";

/** Largest base64 payload per image, matching the backend's check. The raw
 * file cap is derived: base64 inflates every 3 bytes to 4. */
export const MAX_IMAGE_BASE64_BYTES = 5 * 1024 * 1024;

const SUPPORTED_IMAGE_TYPES = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/**
 * Why this file cannot be attached, or null when it can. The same rules the
 * backend enforces, checked before any IPC so the toast is instant.
 */
export function imageRejection(file: {
  type: string;
  size: number;
}): string | null {
  if (!SUPPORTED_IMAGE_TYPES.includes(file.type)) {
    return `Unsupported image type: ${file.type || "unknown"}. Use PNG, JPEG, GIF or WebP.`;
  }
  const base64Length = Math.ceil(file.size / 3) * 4;
  if (base64Length > MAX_IMAGE_BASE64_BYTES) {
    return "Image is too large — the limit is 5 MB per image.";
  }
  return null;
}

/** The `data:` URL the preview thumbnail renders from. */
export function imagePreviewUrl(image: ImagePart): string {
  return `data:${image.media_type};base64,${image.data}`;
}

/** Reads an image file into an `ImagePart`, base64 and all. */
export function readImagePart(file: File): Promise<ImagePart> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const dataUrl = String(reader.result);
      resolve({ media_type: file.type, data: dataUrl.slice(dataUrl.indexOf(",") + 1) });
    };
    reader.onerror = () => reject(reader.error ?? new Error("Could not read the image."));
    reader.readAsDataURL(file);
  });
}
