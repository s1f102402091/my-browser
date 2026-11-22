// Conventional Commits (https://www.conventionalcommits.org/ja/) に従っているか検査する。
// 例: `feat(url): URL をスキーム・ホスト・パスに分解する`
/** @type {import('@commitlint/types').UserConfig} */
export default {
  extends: ["@commitlint/config-conventional"],
  rules: {
    // 日本語の件名は大文字・小文字の概念がないため、件名の書式チェックを無効化する
    "subject-case": [0],
    // 本文の長い URL などで落ちないよう、本文の行長は警告に留める
    "body-max-line-length": [1, "always", 100],
    "footer-max-line-length": [1, "always", 100],
  },
};
