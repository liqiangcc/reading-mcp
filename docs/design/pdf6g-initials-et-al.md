# PDF-6g: multi-letter initials and `et al.` inside the sentence splitter

Step-0 measurement for #232. Baseline: `PDF quality probe` run 44531503850 on
`6992fc3` (product code `origin/main` `9274b1a`; harness-only
`--keep-pdf-sentence-text`), 121 papers. Pricing: `scripts/pdf6f-sentence-price.py
--pdf6g`, whose Python port of `sentence_ranges` reproduces the server's sentence
stream **exactly for 121 of 121 non-truncated documents**, and whose projected
broken-sentence count matches the harness's own document records on every paper
(0 drift).

## The two candidate rules

- **R-I, multi-letter initials.** A period ending a token of single ASCII
  letters joined by periods (`E.F`, `K.R`, `J.R.R`) is not terminal when the
  next non-whitespace character is uppercase — the rule the one-letter check
  already applies.
- **R-A, `et al.`** A period after `al` that directly follows `et` is terminal
  only when the next non-whitespace character is uppercase, like `etc` today:
  `et al. (2019)`, `et al. [13]`, `et al. show`, `et al.,` are not split;
  `… et al. The …` still is.

## Prices (each rule alone, then both)

| Rule | Boundaries removed | Units merged | Papers touched | Short (<5) | Long (>800) | M1 (>5% long) | Broken sentences |
| --- | --- | --- | --- | --- | --- | --- | --- |
| today | - | - | - | 6747 | 1987 | 16 | 6380 |
| R-I initials | 41 | 41 | 14 | 6747 | 1987 | 16 | 6380 |
| R-A et al. | 1849 | 1849 | 58 | 6742 | 1991 | 17 | 6380 |
| R-I+R-A | 1890 | 1890 | 66 | 6742 | 1991 | 17 | 6380 |

Papers above 10% short units: 14 under every rule set. No coarse paragraph's
PDF-6f verdict flips under the new ranges.

`units merged` equals `boundaries removed`: every removed boundary joins the two
units it separated (chained removals merge more than two units into one). The
`et al.` merges add 4 long units and move one paper over the 5% line — a single
references paragraph whose citation sentences collapse into one long unit.

## R-I removed-boundary sample (seed 23; all 35 of the 41 that fit the caps, 14 documents)

Label: `correct_join` = the period was not a sentence end; `wrong_join` = a real
sentence end was merged.

| # | Id | Unit | Context (60 chars each side) | Label |
| --- | --- | --- | --- | --- |
| 1 | 2312.13331 | 22 | `from the Emory Rollins School of Public Health, Harvard T.H. Chan School of Public Health, and the UK Small Area Health` | correct_join |
| 2 | 2403.03967 | 27 | `( iii ) we used that V q > − 1 10 c 4 ( d, τ, k, p ) w.h.p. Similarly, for j ∈ J − , Eq. (13) is at most 9 10 c 4 ( d,` | wrong_join |
| 3 | 2404.07090 | 3 | `which links the interaction and noise matrices A and W [c.f. Eq. (A3)]. If this condition holds, the stationary distribu` | correct_join |
| 4 | 2403.03967 | 27 | `hat ∥ P ζ ( q ) ∥ > gτ 2 2 and ϵ ⊥ ζ ≤ � 13 gτ 2 8 w.h.p. Similarly, for j ∈ J − , Eq. (14) is at most c 6 ( g, τ ) �` | wrong_join |
| 5 | 2311.18111 | 228 | `As usual this equation reduces by half the spinorial d.o.f. Therefore the two states u α ( P ) | α, P ⟩ R and v ˙ α ( P` | wrong_join |
| 6 | 2403.17729 | 12 | `y 𝜏 in the range of 0.6 to 1.6 and show the results ( w.r.t. SASRec) in Figure 6(b). We can observe that too large a val` | correct_join |
| 7 | 2510.18203 | 164 | `here P ( X 1 ∈ A 1 , X 2 ∈ A 2 ) : = P � � ω ∈ Ω s.t. X 1 ( ω ) ∈ A 1 and X 2 ( ω ) ∈ A 2 � � . Relatedly,` | correct_join |
| 8 | 2404.07090 | 15 | `+ d x i � F i ( x ) P ( x ) � . Substituting F i ( x ) [c.f. Eq. (1)] into the latter integral and factorising P ( x ) =` | correct_join |
| 9 | 2510.07408 | 1 | `tefully acknowledges the support and hospitality of the C.N. Yang Institute for Theoretical Physics and the Simons Cente` | correct_join |
| 10 | 2403.02642 | 2 | `ority vote of class predictions: c j k = 1 | G j | � i s.t. P i t ′ → t ∈ G j S i k , (2) y j = arg max k c j k . (3) A` | correct_join |
| 11 | 2510.07408 | 246 | `and an integer n denotes the n th connective cover (a.k.a. Postnikov truncation) deﬁned such that π k ( X ⟨ n ⟩ ) = 0` | correct_join |
| 12 | 2404.07090 | 16 | `the gamma distributions β i are solely determined by W [c.f. Eq. (8)]. On the other hand, rewriting Eq. (A3) as A = − TW` | correct_join |
| 13 | 2404.07090 | 9 | `results that we have obtained in the previous sections (c.f. Fig. 5), this macroecological pattern must be nuanced. To b` | correct_join |
| 14 | 2404.07090 | 6 | `ameters β i = ¯ x 2 i /σ 2 i of the gamma distribution [c.f. Eq. (10)], we finally obtain ¯ x j ( x i ) = ¯ x j � 1 + ρ` | correct_join |
| 15 | 2403.17729 | 13 | `we vary 𝜖 from 5e-6 to 3e-5 and report the results ( w.r.t. SASRec) in Figure 6(a). It demonstrates that an appropriate` | correct_join |
| 16 | 2403.17729 | 13 | `y 𝜏 in the range of 0.6 to 1.6 and show the results ( w.r.t. SASRec) in Figure 6(b). We can observe that too large a val` | correct_join |
| 17 | 2404.07090 | 8 | `alone can explain the variability of the shape factor [c.f. Eq.(27)]. This variability is also present in the empirical` | correct_join |
| 18 | 2510.07408 | 72 | `function with the other spin structure, the bounding (a.k.a. Neveu-Schwarz, a.k.a. anti-periodic) spin structure, then t` | correct_join |
| 19 | 2404.07090 | 7 | `abil- ity density of the re-scaled log-shape parameter [c.f. Eq. (33)] as obtained from the empirical data (blue dots) o` | correct_join |
| 20 | 2404.07090 | 8 | `abil- ity density of the re-scaled log-shape parameter [c.f. Eq. (33)] as obtained from the empirical data (blue dots) o` | correct_join |
| 21 | 2510.18203 | 88 | `nstant, periodic function. Prove that the set � T > 0 s.t. T is a period for f � (2.16.5) admits a minimum T 0 (wh` | correct_join |
| 22 | 2510.18203 | 164 | `f the circle, that is P ( X ∈ A ) : = P � � ω ∈ Ω s.t. X ( ω ) ∈ A � � . In this notation, two random variable` | correct_join |
| 23 | 2403.03967 | 21 | `) , ζ ( j ) ⟩ P | , |⟨ ζ ( i ) , ζ ( j ) ⟩ P o | ≤ δ w.h.p. Specifically, P � ⟨ ζ ( i ) , ζ ( j ) ⟩ P ≥ δ � ≤ 4 e − 2 δ` | wrong_join |
| 24 | 2510.07408 | 316 | `[Cited on: pgs. 239 and 244 .] [Pac91] U. Pachner, P.L. Homeomorphic Manifolds are Equivalent by Elementary Shellin` | correct_join |
| 25 | 2502.07239 | 27 | `2 T 𝑡𝑝𝑠 𝜕𝑥 𝜕𝑦 � 2 + � 𝜕 2 T 𝑡𝑝𝑠 𝜕𝑦 2 � 2 � � � 𝑑𝑥𝑑𝑦, s.t. T 𝑡𝑝𝑠 ( 𝑝 D 𝑖 ) = 𝑝 S 𝑖 , 𝑖 = 1 , 2 , ... , 𝑁 , (11) where` | correct_join |
| 26 | 2307.03827 | 0 | `chanisms ( Wardlaw © 2023 CC-BY 4.0 , A. Ghazvanchahi, P.J. Maralani, A.R. Moody & A. Khademi. arXiv:2307.03827v1 [ee` | correct_join |
| 27 | 2403.17992 | 0 | `jie Zheng, Rafael Fuentes-Dominguez, Matt Clark, George S.D. Gordon, Fernando Perez-Cota Optics and Photonics Research G` | correct_join |
| 28 | 2510.07408 | 72 | `tion Of A Field structure. If it is the nonbounding (a.k.a. Ramond, a.k.a. periodic) spin structure, then the partition` | correct_join |
| 29 | 2311.16343 | 8 | `ch the boundary condition for only ϕ − changes to the D.b.c. Meanwhile, when K < 1 / 2 , the D.b.c.’s should be imposed` | wrong_join |
| 30 | 2307.03827 | 0 | `law © 2023 CC-BY 4.0 , A. Ghazvanchahi, P.J. Maralani, A.R. Moody & A. Khademi. arXiv:2307.03827v1 [eess.IV] 7 Jul 2` | correct_join |
| 31 | 2510.07408 | 134 | `composition with the existence of a Green’s function (a.k.a. Green function [ Wri06 ]) for the Laplacian { d, d † } . A` | correct_join |
| 32 | 2403.03967 | 23 | `bound so that the event 4 of this lemma is satisfied w.h.p. P ( E ) = P { � r ∈ [ k ] | V r | > 1 10 · c 4 ( d, τ, k,` | wrong_join |
| 33 | 2403.17729 | 12 | `we vary 𝜖 from 5e-6 to 3e-5 and report the results ( w.r.t. SASRec) in Figure 6(a). It demonstrates that an appropriate` | correct_join |
| 34 | 2510.18414 | 6 | `e function in Cramér’s large deviations principle for i.i.d. Bernoulli trials with success probability p = 1 = 10 . Ther` | correct_join |
| 35 | 2402.18422 | 5 | `nd Technology, Government of India. AD acknowledges the J.C. Bose Fellowship (JCB/2022/000014) of the Science and Engine` | correct_join |

**R-I verdict: 6 wrong_join of 35 (17%).** The failures are all sentence-final
abbreviations that *look like* initials to the rule — `w.h.p.`, `d.o.f.`,
`D.b.c.` — and in statistics/physics paragraphs they end a sentence more often
than they join one. R-I is **out**.

## R-A removed-boundary sample (seed 23; 50 of 1849, 22 documents, at most 8 per document)

| # | Id | Unit | Context (60 chars each side) | Label |
| --- | --- | --- | --- | --- |
| 1 | 2307.03827 | 1 | `al rating systems such as the Fazekas scale ( Fazekas et al. , 1993 ) or by manual segmentation ( Caligiuri et al. , 2` | correct_join |
| 2 | 2403.03788 | 3 | `, 2023d ) and can do complex logic reasoning ( Feng et al. , 2023 ; Liu et al. , 2023a ), pass human-level examinat` | correct_join |
| 3 | 2402.13288 | 0 | `tructured and clean table data, as highlighted in Shi et al. [2020]. However, real-world scenarios often involve heterog` | correct_join |
| 4 | 2505.21329 | 7 | `n schema matching and structural similar- ity ( Sarma et al. , 2012 ), Nargesian et al. ( 2018 ) formalized TUS by as` | correct_join |
| 5 | 2305.18793 | 380 | `Direct and Indirect Effects Example 27.1 VanderWeele et al. (2012) conducted mediation analysis to assess the extent to` | correct_join |
| 6 | 2402.13288 | 8 | `50.2 TAPEX Liu et al. [2021] 55.5 57.9 OmniTab Jiang et al. [2022] 61.8 62.1 Prompt-based LLMs ChatGPT Cheng et al. [20` | correct_join |
| 7 | 2306.11843 | 14 | `list of predictions. We use the FAIRSEQ toolkit ( Ott et al. , 2019 ) for these experiments. For RAG we use the impleme` | correct_join |
| 8 | 2403.18973 | 4 | `value no higher than seven ( Miller , 1956 ; Plass et al. , 2010 ). The second parameter is the error rate α . It co` | correct_join |
| 9 | 2402.13288 | 3 | `l. [2022]. Generation-based methods like RAT-SQL Wang et al. [2019] directly produce SQL queries using an encoder-decode` | correct_join |
| 10 | 2403.18133 | 6 | `xt paragraph, from the Knowledge Graphs book by Hogan et al. [2021] (see Table 1 for high-level explanations of the nota` | correct_join |
| 11 | 2305.18793 | 94 | `thout assuming the sharp null hypothesis. Instead, Li et al. (2018b) derived the asymptotic distribution of the differen` | correct_join |
| 12 | 2403.03883 | 8 | `en-source frameworks ( Shoeybi et al. , 2019 ; Wolf et al. , 2019 ; Lhoest et al. , 2021 ) utilizing DeepSpeed (leve` | correct_join |
| 13 | 2403.04803 | 3 | `sitive data. Subsequent enhancements by Kone ˇ cn ´ y et al. focused on optimizing communication efﬁciency, a pivotal as` | correct_join |
| 14 | 2306.11843 | 5 | `s DPR with a generator on the downstream task ( Lewis et al. , 2020b ). RAG is shown to achieve good performance on kno` | correct_join |
| 15 | 2403.03883 | 0 | `e.g. translation ( Xu et al. , 2023 ), medical ( Chen et al. , 2023 ), and code gener- ation ( Roziere et al. , 2023 ;` | correct_join |
| 16 | 2505.21329 | 3 | `gy mappings, and natural language embeddings. Bogatu et al. ( 2020 ) incor- porated additional features (e.g., value f` | correct_join |
| 17 | 2305.18793 | 318 | `examples with binary data. Example 22.1 Investigators et al. (2014) assess the effectiveness of the emergency endovascul` | correct_join |
| 18 | 2311.11050 | 1 | `within statistical process monitoring (SPM) ( Stuart et al. , 1996 ), proﬁle monitoring aims at the detection of speci` | correct_join |
| 19 | 2312.16335 | 1 | `ally designed for a sequential access pattern ( André et al. , 2021 ; Guo et al. , 2020 ), rendering them not suitabl` | correct_join |
| 20 | 2307.03827 | 5 | `be optimal for FLAIR-only WML segmentation ( Khademi et al. , 2021 ). SC UNet adds skip connections between the shallo` | correct_join |
| 21 | 2305.18793 | 394 | `doxical and can be called the mediator paradox . Chen et al. (2007) reported a related surrogate endpoint paradox or int` | correct_join |
| 22 | 2402.13288 | 1 | `ed transformer architectures, with sparse Eisenschlos et al. [2021] or biased Golchin and Surdeanu [2023] attention matr` | correct_join |
| 23 | 2603.17845 | 8 | `mper et al. , 2020 ; Naylor et al. , 2018 ; Mahbod et al. , 2024 ; Schuiveling et al. , 2025 ; Vadori et al. , 2` | correct_join |
| 24 | 2312.16335 | 10 | `hang et al. , 2014 ; André et al. , 2015 ; Matsui et al. , 2018 ; Guo et al. , 2020 ; Wang and Deng , 2020 ; J` | correct_join |
| 25 | 2310.11187 | 195 | `ticast.aspra.uni - bamberg.de/resources/wowa ; Haig et al. 2022) is a database containing data on word order on a sa` | correct_join |
| 26 | 2306.11843 | 4 | `representation. For cell ﬁlling, InfoGather ( Yakout et al. , 2012 ) retrieves tables from the table corpus and select` | correct_join |
| 27 | 2311.11050 | 9 | `o handle vanishing or exploding gradients ( Rumelhart et al. , 1986 ); the NN architecture, which includes the number o` | correct_join |
| 28 | 2403.03788 | 0 | `Wob++ ( Liu et al. , 2018 ). Also, recent works ( Zhu et al. , 2023 ; Liu et al. , 2023d ; Wang et al. , 2023c ) su` | correct_join |
| 29 | 2312.01833 | 3 | `dard linear control theory textbooks, e.g. Oppenheim et al. ( 1997 ). If all these conditions are met, the inverse sys` | correct_join |
| 30 | 2403.18133 | 1 | `ent extensive systematic literature review by Kaushik et al. [2023], there are 3 main categories of approaches to NARM:` | correct_join |
| 31 | 2403.01809 | 11 | `12 K. Wolsing et al. accumulate toward the end. Upon investigation of TABOR’s tr` | correct_join |
| 32 | 2603.17845 | 2 | `promptable segmentation on domain-specific data ( Ma et al. , 2024 ; Archit et al. , 2025b ; Cheng et al. , 2023 )` | correct_join |
| 33 | 2403.01809 | 7 | `esults provide further evidence for prior work by Kus et al. [19], who already identiﬁed a lack of generalization during` | correct_join |
| 34 | 2412.16346 | 7 | `LOW et al. : SOUS VIDE: COOKING VISUAL DRONE NAVIGATION POLICIES IN A` | correct_join |
| 35 | 2312.16335 | 10 | `uo et al. , 2020 ; Wang and Deng , 2020 ; Johnson et al. , 2021 ; André et al. , 2021 ; Ko et al. , 2021 ) were` | correct_join |
| 36 | 2403.03967 | 1 | `ustrate this phenomenon with a simple example. Shamir et al. (2022)’s observations also support the phenomenon we descri` | correct_join |
| 37 | 2402.13288 | 8 | `1.5 - TableFormer Yang et al. [2022] 52.6 - GRAPPA Yu et al. [2020] 52.7 - DoT Krichene et al. [2021] 54.0 - REASTAP Zha` | correct_join |
| 38 | 2403.03883 | 8 | `lizing DeepSpeed (level 3) with Flash attention ( Dao et al. , 2022 ; Dao , 2023 ). It is built on PyTorch ( Paszke e` | correct_join |
| 39 | 2506.06561 | 5 | `tently quality’s caption from arXiv reference ( Huang et al. , 2023 ), reducing the perceived quality. 5 Discussion Our` | correct_join |
| 40 | 2403.09706 | 21 | `s have been presented ( Lin et al. , 2020 ; Scholak et al. , 2021 ). The performance in the simple or single-table qu` | correct_join |
| 41 | 2402.13288 | 8 | `54.0 - REASTAP Zhao et al. [2022] 58.6 - TaCube Zhou et al. [2022b] 60.8 - TAPAS Herzig et al. [2020] 48.8 50.2 TAPEX L` | correct_join |
| 42 | 2506.06561 | 3 | `sing all available proﬁle(s) from the same paper. et al. , 2002 ) and ROUGE ( Lin , 2004 ). 3 We used reference-ba` | correct_join |
| 43 | 2306.11843 | 3 | `n beyond Wikipedia. This dataset, based on Cafarella et al. ( 2008 ), is also larger and more diverse than the standar` | correct_join |
| 44 | 2403.03883 | 1 | `fully used in var- ious ﬁelds such as medicine ( Chen et al. , 2023 ; Ji et al. , 2023 ), translation ( Xu et al. ,` | correct_join |
| 45 | 2603.17845 | 5 | `ed extensively for microscopy data (see e.g. ( Archit et al. , 2025a ), Sec. 4 ). It yields good segmentation results` | correct_join |
| 46 | 2610.01064 | 17 | `et al. , 2024b ; Boutaleb et al. , 2025 ; Agarwal et al. , 2025 ) add structural signals during retrieval or re-ran` | correct_join |
| 47 | 2305.18793 | 395 | `mediation analysis VanderWeele (2008) and Forastiere et al. (2018) reviewed and compared prin- cipal stratification and` | correct_join |
| 48 | 2502.13422 | 2 | `cting a sub-table at each reason- ing step. DATER (Ye et al. 2023) follows a similar idea, de- composing both the origin` | correct_join |
| 49 | 2505.21329 | 21 | `a- tion, following LLM-based methods like U GEN ( Pal et al. , 2024 ), to veriﬁably embed semantic con- straints or dom` | correct_join |
| 50 | 2403.04803 | 3 | `across diverse domains [15], [16]. McMahan et al. laid the cornerstone of FL, proposing a framework that mini` | correct_join |

**R-A verdict: 0 wrong_join of 50.** Every sampled `et al.` period is
mid-citation — the period after `al` is never the sentence end the reader wants.
R-A **ships**, alone.

## Out of scope (unchanged)

Spaced decimals (`( 0 . 63`), `Rev.`/`Phys.` journal abbreviations, `Ph.D.`,
everything else. R-I is out for cause, listed above; `E.F.`/`K.R.` keep
splitting exactly as before this change.
