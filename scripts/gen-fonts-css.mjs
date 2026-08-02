// src/styles/fonts.css 를 생성합니다.
//
//   node scripts/gen-fonts-css.mjs
//
// 왜 직접 만드는가 —
// @fontsource 의 `400.css` 는 한글을 120여 개 subset 으로 쪼개 놓습니다. 정확하지만
// 웹폰트를 네트워크로 받는 상황을 가정한 구성이라, 로컬 디스크에서 읽는 데스크탑
// 앱에는 파일 수(374개)와 설치본 크기(9MB)만 늘립니다.
//
// 반대로 `korean-400.css` 는 한 덩어리라 가볍지만 `unicode-range` 가 없어서,
// 같은 family/weight 인 `latin-400.css` 와 함께 쓰면 나중에 선언된 쪽이 전부를
// 가져가 버립니다(라틴 글자가 한글 subset 에서 찾아지지 않아 폴백됨).
//
// 그래서 여기서 통합 한글 파일에 **정확한 unicode-range** 를 붙여 줍니다.
// 범위는 fontsource 가 생성한 숫자 subset 들의 합집합에서 그대로 가져오므로
// 손으로 추측하지 않습니다.

import { readFileSync, writeFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const SANS = 'ibm-plex-sans-kr'
const WEIGHTS = [400, 500, 600] // 목업이 쓰는 굵기. 700 은 어디에도 안 씁니다.

/** 숫자 subset(= 한글/CJK)들의 unicode-range 만 모읍니다. latin 계열은 제외. */
function koreanRanges(weight) {
  const css = readFileSync(resolve(ROOT, 'node_modules/@fontsource', SANS, `${weight}.css`), 'utf8')
  const blocks = css.split('@font-face')
  const ranges = []

  for (const block of blocks) {
    // `/* ibm-plex-sans-kr-[0]-400-normal */` 처럼 숫자로 된 subset 만 대상.
    const name = /ibm-plex-sans-kr-\[([^\]]+)\]/.exec(block)?.[1]
    if (!name || !/^\d+$/.test(name)) continue
    const found = /unicode-range:\s*([^;]+);/.exec(block)?.[1]
    if (found) ranges.push(found.trim())
  }
  return ranges.join(',')
}

const parts = [
  '/* GENERATED — scripts/gen-fonts-css.mjs 로 다시 만드세요. 직접 고치지 마세요. */',
  '/* 사내망/오프라인에서도 목업과 같은 조판이 나오도록 폰트를 번들에 넣습니다. */',
  '',
]

for (const weight of WEIGHTS) {
  // 라틴이 먼저. unicode-range 가 없으므로 "나머지 전부"를 맡습니다.
  parts.push(
    `@font-face {`,
    `  font-family: 'IBM Plex Sans KR';`,
    `  font-style: normal;`,
    `  font-weight: ${weight};`,
    `  font-display: swap;`,
    `  src: url('@fontsource/${SANS}/files/${SANS}-latin-${weight}-normal.woff2') format('woff2');`,
    `}`,
    '',
  )
  // 한글이 나중. 명시된 범위 안에서는 이쪽이 이깁니다.
  parts.push(
    `@font-face {`,
    `  font-family: 'IBM Plex Sans KR';`,
    `  font-style: normal;`,
    `  font-weight: ${weight};`,
    `  font-display: swap;`,
    `  src: url('@fontsource/${SANS}/files/${SANS}-korean-${weight}-normal.woff2') format('woff2');`,
    `  unicode-range: ${koreanRanges(weight)};`,
    `}`,
    '',
  )
}

// 코드/수치용. 한글이 없는 폰트라 라틴 subset 하나면 충분합니다.
for (const weight of [400, 500]) {
  parts.push(
    `@font-face {`,
    `  font-family: 'JetBrains Mono';`,
    `  font-style: normal;`,
    `  font-weight: ${weight};`,
    `  font-display: swap;`,
    `  src: url('@fontsource/jetbrains-mono/files/jetbrains-mono-latin-${weight}-normal.woff2') format('woff2');`,
    `}`,
    '',
  )
}

const out = resolve(ROOT, 'src/styles/fonts.css')
writeFileSync(out, parts.join('\n'), 'utf8')
console.log(`fonts.css → ${WEIGHTS.length * 2 + 2} faces`)
