import test from "node:test";
import assert from "node:assert/strict";

import {
  fileReferenceInsertion,
  fileReferenceToken,
  matchFileReferences,
} from "./fileReferences.ts";

test("fileReferenceToken finds an @ token at a word boundary", () => {
  assert.deepEqual(fileReferenceToken("@", 1), { start: 0, query: "" });
  assert.deepEqual(fileReferenceToken("@src/foo", 8), {
    start: 0,
    query: "src/foo",
  });
  assert.deepEqual(fileReferenceToken("see @pack", 9), {
    start: 4,
    query: "pack",
  });
  // the token may contain path separators but not whitespace or another @
  assert.deepEqual(fileReferenceToken("@a/b/c.ts", 9), {
    start: 0,
    query: "a/b/c.ts",
  });
});

test("fileReferenceToken ignores email-like and finished tokens", () => {
  assert.equal(fileReferenceToken("a@b.com", 6), null);
  assert.equal(fileReferenceToken("issue@42", 8), null);
  // whitespace closes the token
  assert.equal(fileReferenceToken("@src other", 10), null);
  // the caret sits before the @
  assert.equal(fileReferenceToken("@src", 0), null);
});

test("matchFileReferences filters paths case-insensitively, directories last", () => {
  const files = [
    "src/store.ts",
    "src-tauri/src/store.rs",
    "README.md",
    "src/",
  ];
  assert.deepEqual(matchFileReferences(files, "store"), [
    "src-tauri/src/store.rs",
    "src/store.ts",
  ]);
  // empty query: files first, directories last
  assert.deepEqual(matchFileReferences(files, "").slice(0, 2), [
    "README.md",
    "src-tauri/src/store.rs",
  ]);
  assert.equal(matchFileReferences(files, "").at(-1), "src/");
  // query with a slash matches anywhere in the path
  assert.deepEqual(matchFileReferences(files, "ri/src"), [
    "src-tauri/src/store.rs",
  ]);
  assert.deepEqual(matchFileReferences(files, "zzz"), []);
});

test("fileReferenceInsertion wraps paths with spaces", () => {
  assert.equal(fileReferenceInsertion("src/foo.rs"), "@src/foo.rs ");
  assert.equal(fileReferenceInsertion("my file.txt"), '@"my file.txt" ');
});
