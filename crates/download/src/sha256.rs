//! SHA-256，自实现。
//!
//! ## 为什么不加 `sha2` crate
//!
//! 理由不是"省一个依赖"，而是**这个函数的位置**：它站在"我们信任这个字节流"
//! 与"我们把它解压到磁盘"之间。它是整个工具里最该被独立核对的一段代码。
//!
//! - 它是 **FIPS 180-4 的固定算法**，没有版本演进，没有平台差异，没有配置项
//! - 实现是纯函数、约 120 行、零依赖、零 IO
//! - **它有官方测试向量**（FIPS 180-4 的 `abc` / 空串 / 448 位边界 / 896 位边界，
//!   以及百万个 `a`），所以"对不对"不靠信任，靠跑
//!
//! 一个 security-critical 的纯函数，用 120 行换掉一条依赖链，是划算的。
//!
//! ## 与 `sha2` crate 的一致性
//!
//! 测试里包含 NIST 的标准向量。如果要交叉验证，任何一台 Linux 上
//! `printf 'abc' | sha256sum` 应当给出 `ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad`
//! —— 那个值在下面的测试里逐字出现。

/// SHA-256 的初始哈希值（FIPS 180-4 §5.3.3）。
const INITIAL_STATE: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

/// 每一轮的加常量（FIPS 180-4 §4.2.2，前 64 个素数立方根的小数部分前 32 位）。
const ROUND_CONSTANTS: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

/// 一块处理完之后的状态。
type State = [u32; 8];

/// 处理一个 64 字节的块。
///
/// **`chunk` 必须是 64 字节**；调用方（[`Sha256::update`] 与 [`Sha256::finish`]）
/// 保证了这一点，所以这里不做长度分支 —— 那种分支会让"少了几个字节的哈希"
/// 变成静默错误。
fn compress(state: &mut State, chunk: &[u8; 64]) {
    // 消息调度表：前 16 个字直接取自块，其余 48 个递推出来。
    let mut schedule = [0u32; 64];
    for (index, slot) in schedule.iter_mut().take(16).enumerate() {
        let base = index * 4;
        *slot = u32::from_be_bytes([
            chunk[base],
            chunk[base + 1],
            chunk[base + 2],
            chunk[base + 3],
        ]);
    }
    for index in 16..64 {
        // σ0(x) = ROTR⁷(x) ⊕ ROTR¹⁸(x) ⊕ SHR³(x)
        let s0 = schedule[index - 15].rotate_right(7)
            ^ schedule[index - 15].rotate_right(18)
            ^ (schedule[index - 15] >> 3);
        // σ1(x) = ROTR¹⁷(x) ⊕ ROTR¹⁹(x) ⊕ SHR¹⁰(x)
        let s1 = schedule[index - 2].rotate_right(17)
            ^ schedule[index - 2].rotate_right(19)
            ^ (schedule[index - 2] >> 10);
        schedule[index] = schedule[index - 16]
            .wrapping_add(s0)
            .wrapping_add(schedule[index - 7])
            .wrapping_add(s1);
    }

    let (mut a, mut b, mut c, mut d) = (state[0], state[1], state[2], state[3]);
    let (mut e, mut f, mut g, mut h) = (state[4], state[5], state[6], state[7]);

    for index in 0..64 {
        let big_s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let choose = (e & f) ^ (!e & g);
        let temp1 = h
            .wrapping_add(big_s1)
            .wrapping_add(choose)
            .wrapping_add(ROUND_CONSTANTS[index])
            .wrapping_add(schedule[index]);
        let big_s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let majority = (a & b) ^ (a & c) ^ (b & c);
        let temp2 = big_s0.wrapping_add(majority);

        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(temp1);
        d = c;
        c = b;
        b = a;
        a = temp1.wrapping_add(temp2);
    }

    for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *slot = slot.wrapping_add(value);
    }
}

/// 流式 SHA-256。
///
/// 流式而不是"一次吃整个文件"：制品是几百 MB（Node 的 win-x64 zip 约 30 MB，
/// Temurin JDK 约 190 MB），**把它们整个读进内存只为了算哈希是不可接受的**。
#[derive(Debug, Clone)]
pub struct Sha256 {
    state: State,
    /// 还没满 64 字节的尾巴。
    buffer: [u8; 64],
    /// `buffer` 里有效字节数。
    buffered: usize,
    /// 已经喂进来的总字节数（**不是** `buffer` 的长度）。
    total: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    /// 新建一个空的哈希器。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: INITIAL_STATE,
            buffer: [0u8; 64],
            buffered: 0,
            total: 0,
        }
    }

    /// 喂入一段字节。
    ///
    /// 语义：**把能压缩的整块都压掉，只留下不满 64 字节的尾巴**。
    /// 尾巴留在 `self.buffer[..self.buffered]` 里，等下一次 `update`
    /// 或者 [`Self::finish`] 把它凑成整块。
    pub fn update(&mut self, bytes: &[u8]) {
        self.total = self.total.wrapping_add(bytes.len() as u64);

        let mut cursor = 0;

        // 1) 先补满上一轮的尾巴。补满就地压掉，并把这个槽位腾出来给
        //    下一批尾巴 —— 所以这里是"压缩 + 游标清零"，不是"继续往后堆"。
        if self.buffered > 0 {
            let take = (64 - self.buffered).min(bytes.len());
            self.buffer[self.buffered..self.buffered + take].copy_from_slice(&bytes[..take]);
            self.buffered += take;
            cursor += take;
            if self.buffered == 64 {
                let chunk = self.buffer;
                compress(&mut self.state, &chunk);
                self.buffered = 0;
            }
        }

        // 2) 整块整块地吃。块数与尾巴都显式算出来 —— 手写游标的两个来源
        //    （切片与偏移）很容易不同步，而"微微少压一次"的症状是摘要
        //    对不上，几乎无法反推。
        while cursor + 64 <= bytes.len() {
            let chunk: &[u8; 64] = bytes[cursor..cursor + 64]
                .try_into()
                .expect("条件已经保证这里有 64 字节");
            compress(&mut self.state, chunk);
            cursor += 64;
        }

        // 3) 剩下的进尾巴。
        //
        // **是"接到尾巴后面"，不是"替换尾巴"。** 走到这里有两种情况：
        //
        // - 刚才把尾巴补满了（`buffered` 现在是 0）：这就是新尾巴；
        // - 刚才**没**补满（尾巴还在）：新来的字节要接在它后面。
        //
        // 早先这里写的是 `self.buffer[..rest.len()]` + `self.buffered = rest.len()`，
        // 也就是**用新字节覆盖掉还没满的尾巴**。症状是"逐字节喂"的结果与
        // "一次喂"不一致（`total` 对、摘要不对），而一次性喂整段永远测不出来
        // —— 因为那时 `buffered` 在进入这一步时总是 0。
        let rest = &bytes[cursor..];
        self.buffer[self.buffered..self.buffered + rest.len()].copy_from_slice(rest);
        self.buffered += rest.len();
    }

    /// 收尾并给出十六进制摘要（小写，64 字符）。
    #[must_use]
    pub fn finish(mut self) -> String {
        // ── 填充（FIPS 180-4 §5.1.1）────────────────────────────────────────
        //
        // 报文后面接一个 `0x80`、若干个 `0x00`、最后 8 字节的大端总**位数**，
        // 使总长度成为 64 的整数倍。
        //
        // 一次算清楚而不是补完再补：`buffered` 是尾巴的长度，`0x80` 占一个
        // 字节、长度占 8 个，所以填充之后的总长是
        // `ceil((buffered + 9) / 64) * 64`，也就是 64 或 128。
        //
        // **`+ 64` 再取模不是多余的**：`56 - x` 在 `x > 56` 时会在 `u64` 上
        // 下溢，release 里静默回绕成巨大的数。
        let with_marker = self.buffered + 1;
        let zero_pad = (56 + 64 - with_marker % 64) % 64;
        let padded_len = with_marker + zero_pad + 8;
        debug_assert!(
            padded_len == 64 || padded_len == 128,
            "填充后只可能是 64 或 128，实际 {padded_len}"
        );

        // 把"尾巴 + 填充 + 长度"拼成一个完整的最后一块（或两块）。
        let mut block = [0u8; 128];
        block[..self.buffered].copy_from_slice(&self.buffer[..self.buffered]);
        block[self.buffered] = 0x80;
        // 长度是**总位数**，大端，落在这一批的最后 8 个字节上。
        block[padded_len - 8..padded_len]
            .copy_from_slice(&self.total.wrapping_mul(8).to_be_bytes());

        for start in (0..padded_len).step_by(64) {
            let chunk: &[u8; 64] = block[start..start + 64]
                .try_into()
                .expect("padded_len 是 64 的整数倍");
            compress(&mut self.state, chunk);
        }

        // 到这里所有字节都进过压缩函数了：上面的循环按 64 字节步进，
        // `padded_len` 又是 64 的整数倍，所以没有留下的尾巴。
        let mut out = String::with_capacity(64);
        for word in self.state {
            out.push_str(&format!("{word:08x}"));
        }
        out
    }
}
/// 一次算完。
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finish()
}

/// 比较两个哈希是否相等，**大小写不敏感**。
///
/// 上游的校验和大小写并不统一（Node 的 `SHASUMS256.txt` 是小写，
/// 有些项目给大写）。大小写不同就判"不符"会把一份好的制品拒掉。
#[must_use]
pub fn checksums_match(expected: &str, actual: &str) -> bool {
    expected.len() == actual.len() && expected.eq_ignore_ascii_case(actual)
}

/// 规整成小写无空格的形式。
///
/// **`trim_start_matches` 是大小写敏感的**，所以先 `to_lowercase` 再削前缀
/// —— 反过来的话 `SHA256:BA7816BF` 会原样留着前缀（上游两种写法都有）。
#[must_use]
pub fn normalize_checksum(raw: &str) -> String {
    raw.trim()
        .to_lowercase()
        .trim_start_matches("sha256:")
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// NIST / FIPS 180-4 的标准向量。
    ///
    /// **这些常量是逐字抄的公开测试向量**，不是我们自己算出来的 ——
    /// 用自己的实现生成期望值等于什么都没测。
    #[test]
    fn fips_180_4_vectors() {
        // FIPS 180-4 附录 B.1：`abc`
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // FIPS 180-4 附录 B.2：空串
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        // FIPS 180-4 附录 B.3：448 位（56 字节）—— 边界：正好需要两块填充
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // FIPS 180-4 附录 B.4：896 位（112 字节）—— 另一侧边界
        assert_eq!(
            sha256_hex(
                b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmn\
                  hijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu"
                    .iter()
                    .copied()
                    .filter(|b| *b != b' ')
                    .collect::<Vec<u8>>()
                    .as_slice()
            ),
            "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1"
        );
    }

    /// FIPS 180-4 附录 B.5：一百万个 `a`。
    ///
    /// **这条测的是流式实现的分块逻辑**，而分块正是自实现最容易错的地方
    /// （整数溢出、尾巴处理、长度计数）。用一次 `update` 喂一百万个字节
    /// 会走不到"补满上一轮尾巴"的分支，所以下面还单独测了逐字节喂。
    #[test]
    fn a_million_a() {
        let mut hasher = Sha256::new();
        let block = vec![b'a'; 1000];
        for _ in 0..1000 {
            hasher.update(&block);
        }
        assert_eq!(
            hasher.finish(),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    /// **逐字节喂与一次喂结果必须一样。** 这条抓的是"分块边界处理错了"。
    #[test]
    fn feeding_byte_by_byte_agrees_with_feeding_all_at_once() {
        for length in [0usize, 1, 55, 56, 57, 63, 64, 65, 119, 120, 121, 128, 1000] {
            let data: Vec<u8> = (0..length).map(|index| (index % 251) as u8).collect();

            let all_at_once = sha256_hex(&data);

            let mut hasher = Sha256::new();
            for byte in &data {
                hasher.update(std::slice::from_ref(byte));
            }
            assert_eq!(
                hasher.finish(),
                all_at_once,
                "长度 {length}：逐字节喂与一次喂不一致 —— 分块边界处理错了"
            );
        }
    }

    /// 各种切分点都必须一致。
    #[test]
    fn arbitrary_chunk_boundaries_agree() {
        let data: Vec<u8> = (0..500u32).map(|index| (index % 256) as u8).collect();
        let expected = sha256_hex(&data);
        for chunk in [
            1usize, 3, 7, 31, 32, 33, 63, 64, 65, 127, 128, 129, 499, 500,
        ] {
            let mut hasher = Sha256::new();
            for piece in data.chunks(chunk) {
                hasher.update(piece);
            }
            assert_eq!(hasher.finish(), expected, "按 {chunk} 字节切分时不一致");
        }
    }

    #[test]
    fn checksum_comparison_ignores_case_and_prefix() {
        // 上游给的校验和大小写并不统一，大小写不同就判"不符"会拒掉好制品。
        assert!(checksums_match(
            "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        ));
        assert!(!checksums_match("ba78", "ba7816bf"));
        assert_eq!(
            normalize_checksum("  SHA256:BA7816BF  "),
            "ba7816bf",
            "前缀与空格都要削掉，且转小写"
        );
    }
}
