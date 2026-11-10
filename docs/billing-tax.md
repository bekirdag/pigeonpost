# VAT and invoice wording for web sales

Decision D-34, 2026-11-04. The owner delegated it; the accountant still has to confirm it (see the
Turkish note in §6). This file holds no secrets.

## 1. Scope

- **Seller of record:** Piyote Bilişim ve Danışmanlık Hizmetleri Ticaret Ltd. Şti. It is a Turkish
  company with no establishment in the EU or the UK.
- **What this covers:** web checkout on `pigeonpost.dev` and `enterprise.pigeonpost.dev`: iyzico
  subscriptions through saas_be, in USD, EUR and TRY. Nest packages (hosted AI agents, monthly)
  are sold here too, through the same bridge path.
- **Company buyers of Nest packages** are business buyers, like an enterprise organization: rule
  (b) in §3 applies when they give a verified VAT number.
- **Out of scope: App Store and Google Play purchases.** Apple and Google are the sellers there and
  account for VAT themselves. This includes Nest packages bought in the iOS or Android app.

## 2. The rules, with sources

| # | Rule | Source |
|---|---|---|
| R1 | Electronically supplied services to a consumer are taxed where the consumer lives. | Council Directive 2006/112/EC Art. 58(1)(c) |
| R2 | **No threshold for a non-EU seller.** The EUR 10,000 cross-border threshold applies only to a supplier established in a single Member State, so a Turkish seller owes destination VAT from the first EU consumer sale. | Art. 58(2)(a) |
| R3 | A non-EU seller declares and pays that VAT through the **non-Union OSS** scheme, registering in one Member State of identification of its choice. It files quarterly returns, with no threshold. | Art. 358a(1), Art. 359; [Commission OSS portal](https://vat-one-stop-shop.ec.europa.eu/one-stop-shop/declare-and-pay-oss_en) |
| R4 | The customer's location rests on **two items of non-contradictory evidence**: billing address, IP address/geolocation, bank or card country, SIM country, fixed line, or other commercially relevant information. The single-item relief (up to EUR 100,000) is only for EU-established suppliers. | Implementing Reg. 282/2011 Art. 24b(d), Art. 24f |
| R5 | **B2B:** the place of supply is the customer's establishment, and the customer pays the VAT (**reverse charge**). The invoice must say "Reverse charge". | Directive Art. 44, Art. 196, Art. 226(11a) |
| R6 | A customer counts as a business only if it gives a VAT number and the supplier verifies it (VIES, Reg. 904/2010 Art. 31). For e-services, a customer who gives no VAT number may be treated as a consumer. | Reg. 282/2011 Art. 18(1)(a), 18(2) |
| R7 | **UK consumers:** a non-UK seller of digital services to UK consumers must register for UK VAT, with no threshold for a non-established taxable person. **UK business customers** with a valid VAT number account for the VAT themselves (reverse charge, VAT Act 1994 s.8). | [HMRC: VAT rules for supplies of digital services to consumers](https://www.gov.uk/guidance/the-vat-rules-if-you-supply-digital-services-to-private-consumers); [VAT Notice 700/1](https://www.gov.uk/government/publications/vat-notice-7001-should-i-be-registered-for-vat/vat-notice-7001-should-i-be-registered-for-vat) §9 (NETPs: any taxable supply, regardless of value); [VATREG37100](https://www.gov.uk/hmrc-internal-manuals/vat-registration-manual/vatreg37100) |
| R8 | **Türkiye:** services provided for customers abroad are exempt from KDV (KDVK 11/1-a). | 3065 sayılı KDV Kanunu [md. 11/1-a](https://www.mevzuat.gov.tr/mevzuatmetin/1.5.3065.pdf) |
| R9 | **Türkiye, 11/1-a conditions:** the customer is abroad (residence, business or legal seat), the service is provided for that customer, and the customer benefits from it abroad. The invoice is issued to the foreign customer. | KDVK md. 12/2; KDV Genel Uygulama Tebliği II/A-5 |
| R10 | **e-Arşiv for foreign buyers:** in common practice, the recipient tax number is `2222222222` for a foreign company and `11111111111` for a person without a TCKN. The exemption code is **302** (11/1-a service export). These come from practitioner guides, not the GİB guide itself, so the accountant must confirm them. | e.g. [Paraşüt](https://www.parasut.com/blog/ihracatta-e-fatura-earsiv-uygulamasi), [TÜRMOB e-Dönüşüm SSS](https://www.turmob.org.tr/e-donusum/efatura/sss) |

The rules were read from the Directive and Regulation texts as published on legislation.gov.uk
(pre-2021 consolidation; Art. 359 was later widened to all B2C services), and from HMRC and
mevzuat.gov.tr on 2026-11-04.

## 3. Decision

| Buyer | Until OSS and UK VAT registration exist | Later (config flip) |
|---|---|---|
| (a) Consumer located in the EU or UK | **Not sold on the web.** Directed to the iOS/Android app (Apple or Google is the seller), or to buy as a business. | `TAX_B2C_OPEN_REGIONS=EU` (and/or `UK`): sold with VAT included at the buyer country's rate (`store-adapter/catalog/vat-rates.json`). |
| (b) Business in the EU or UK | Sold **without VAT**, with a VAT number verified server-side (VIES for the EU, HMRC for the UK) and reverse-charge wording. | Same. |
| (c) Everyone else, Türkiye included | Unchanged. | Unchanged. |

Web prices are VAT-inclusive list prices. Opening a consumer region therefore lowers Piyote's net
revenue by the VAT included at that country's rate. Prices are not raised.

## 4. What the code does

All of it is in the Pigeonpost billing bridge (`store-adapter/`), so it applies only to the
pigeonpost tenant. TNL shares saas_be and the iyzico merchant, and nothing in saas_be changed.

**Where:** `src/tax.js`, called by `individualCheckout` (which serves `cloudCheckout` and
`nestCheckout`) and by `enterpriseCheckout` in `src/billing.js`. The test "Nest checkouts follow the
EU/UK VAT rule and take no store-plan confirmation" in `test/billing.test.mjs` covers Nests.
It runs before any intent is reserved, any org is drafted, or any provider is called.

**Evidence:**
1. **Billing country** of the saas_be billing profile chosen for the checkout. It is read as a
   two-letter code or as a country name in English, Turkish, German, French, Spanish, Italian or
   Dutch. An unrecognised country is refused with `400 billing_country_unknown`.
2. **IP country** from Cloudflare's `CF-IPCountry` header (`TAX_IP_COUNTRY_HEADER`). `pigeonpost.dev`
   is behind Cloudflare. `XX` and Tor (`T1`) count as unknown.
3. **Card country** is not known before iyzico's hosted form, so it is not used. To stay safe, the
   rule is stricter than R4: **either item in the EU/UK makes the buyer an EU/UK buyer**.
   - Exception: a Türkiye billing profile carrying a 10–11 digit Turkish identity or tax number stays
     Turkish on an EU network ("other commercially relevant information", Art. 24f(f)).

**Treatments** (stored with the checkout intent and its subscription in the bridge state):

| Treatment | When | Result |
|---|---|---|
| `standard` | Not EU/UK. | Unchanged checkout. |
| `reverse_charge` | EU/UK, the profile is a **Company** whose VAT number is valid and from the billing country (or the IP country, when the billing country is outside the EU/UK). | Checkout continues. Stored with the VAT number, the registered name, the check time and the source (`vies` or `hmrc`). |
| `b2c_vat` | EU/UK consumer, region listed in `TAX_B2C_OPEN_REGIONS`. | Checkout continues. Stored with country and rate. |
| refusal | EU/UK consumer with the region closed. | `403 eu_uk_consumer_web_unavailable`; the checkout page shows App Store and Google Play links. |

**VAT number checks:**
- **EU:** the VIES REST API (`POST /taxation_customs/vies/rest-api/check-vat-number`, no
  credentials). Greece uses the `EL` prefix.
- **UK:** HMRC "Check a UK VAT number" v2.0, OAuth client credentials. Set `HMRC_CLIENT_ID` and
  `HMRC_CLIENT_SECRET` from an HMRC Developer Hub production application. Until they are set, UK
  businesses get `403 uk_vat_check_unavailable` and the app links.
- **Outcomes:** an invalid number gives `400 vat_id_invalid`. A service outage gives
  `503 vat_check_unavailable`; it never fails open. Valid answers are cached for 24 hours.

**Receipt wording.** `GET /api/v1/cloud/checkout/<intent>`, `GET /api/v1/cloud/subscriptions/<id>`
and `GET /api/ent/checkout/<org>` return a `receipt` object, and the account page shows it under
**Details**:
- seller;
- customer name (company name for a Company profile), country and VAT number;
- these lines:
  - EU B2B: `Reverse charge – VAT to be accounted for by the recipient, Art. 196 Council Directive 2006/112/EC.`
  - UK B2B: `Reverse charge – the customer accounts for UK VAT (VAT Act 1994, section 8).`
  - Any buyer whose billing country is not Türkiye (except `b2c_vat`): `Hizmet ihracı – KDV Kanunu md. 11/1-a uyarınca KDV'den istisnadır (service export, exempt from Turkish VAT).`
  - `b2c_vat`: `Price includes N% VAT (CC).`

Subscriptions started before this change have no stored decision and no `receipt`.

**Configuration** (`/etc/pigeonpost-store-adapter.env`):

| Var | Default | Meaning |
|---|---|---|
| `TAX_EU_UK_GATE` | `on` | `off` disables the whole rule. Only for an emergency, with the accountant's agreement. |
| `TAX_B2C_OPEN_REGIONS` | empty | `EU`, `UK` or `EU,UK` once the matching registration exists. |
| `TAX_IP_COUNTRY_HEADER` | `cf-ipcountry` | Header carrying the edge's IP country. |
| `HMRC_CLIENT_ID`, `HMRC_CLIENT_SECRET`, `HMRC_API_BASE` | unset, unset, `https://api.service.hmrc.gov.uk` | UK VAT number checks. |
| `VIES_CHECK_URL`, `VAT_CHECK_TIMEOUT_MS` | Commission URL, `10000` | EU checks. |

**Rates table:** `store-adapter/catalog/vat-rates.json` holds the standard rates for the 27 EU
countries and GB, as of 2026-11-04, with no 2026 standard-rate changes. Review each rate against the
Commission's TEDB before opening a region, and again when a country announces a change.

### What saas_be issues today (found 2026-11-04)

- **Billing profiles:** saas_be stores the account type, legal and company name, a single `tax_id`
  (a VAT number sent as `vat_number` lands there), and the address with a free-text country.
- **Legal invoices:** saas_be is wired to **Turkcell e-Şirket** for e-Arşiv/e-Fatura. For the
  pigeonpost tenant both are configured in **`test` mode**, so saas_be issues **no live legal
  invoice** for Pigeonpost web sales today, and its invoice download answers 409 until a live
  e-document exists.
  - When a live e-Arşiv is issued, saas_be sends foreign buyers as `2222222222`.
  - It sends one tenant-wide KDV rate.
  - It sends **no exemption code and no notes** (`notes: []`).
  - It does not distinguish reverse charge or export.
- **What follows:** the legal invoice is effectively external to the code (the accountant, or a
  future live Turkcell setup). The wording guidance for it is in §6. Switching e-Arşiv to live for
  Pigeonpost needs a saas_be change first: a per-invoice exemption code 302 with 0% KDV for foreign
  buyers, and the note text above. Otherwise it would issue foreign invoices with Turkish KDV.

## 5. Operations

- Deploy is the normal store-adapter deploy. Back up `src/`, `catalog/` and the env, install, then
  restart `pigeonpost-store-adapter`. No new env is needed for the default behaviour.
- Verify with no charge:
  - `node -e` against `src/tax.js` for the decision table;
  - a checkout initialisation from a non-EU profile, which only creates a hosted form; no card is
    entered and the form expires unused.
- **To open EU consumer sales after OSS registration:** back up the env, add
  `TAX_B2C_OPEN_REGIONS=EU`, and restart. Do the same with `UK` after UK VAT registration. Tell the
  accountant the date.

## 6. Muhasebeci için not (Türkçe)

Pigeonpost web satışları (pigeonpost.dev, iyzico abonelik, USD/EUR/TRY) için 4 Kasım 2026 itibarıyla
şu kural uygulanıyor:

- AB veya Birleşik Krallık'taki **tüketicilere web'den satış yapılmıyor**. Bu kişiler uygulama
  mağazalarından alıyor; orada KDV'yi Apple/Google üstleniyor.
- AB/İngiltere'deki **şirketlere**, VIES/HMRC'de doğrulanan KDV numarasıyla **KDV'siz (reverse
  charge)** satış yapılıyor.
- Diğer herkes için değişiklik yok.

Lütfen şunları teyit edin:

1. **AB non-Union OSS kaydı:** Kayıt yapılacak mı? Yapılacaksa hangi üye ülke (Member State of
   identification) seçilmeli? Örneğin İrlanda, Hollanda veya Almanya. Kayıt tarihi bize iletilirse
   AB tüketici satışı yapılandırmayla açılır. Fiyatlar KDV dahil kabul edilir, oranlar
   `store-adapter/catalog/vat-rates.json` tablosundadır.
2. **Birleşik Krallık KDV kaydı** (NETP, eşik yok): Yapılacak mı? Yapılmayacaksa İngiltere'deki
   tüketicilere web satışı kapalı kalır. Ayrıca şirket alıcıların doğrulaması için HMRC Developer Hub
   uygulama kimlik bilgileri gerekir. Bunlar gelene kadar İngiltere şirketlerine de web satışı
   kapalıdır.
3. **KDVK 11/1-a hizmet ihracı istisnası:** Yurt dışındaki alıcılara (şirket ve tüketici) satılan
   abonelik için istisna uygulanabilir mi? Hangi belgeler saklanmalı?
   - Bizde saklananlar: fatura adresi ülkesi, IP ülkesi, KDV numarası ve VIES/HMRC sorgu sonucu.
   - Ödemeler iyzico üzerinden döviz veya TL olarak geliyor. Döviz bozdurma ya da döviz alım belgesi
     şartı açısından durum nedir?
4. **Yurt dışı alıcılar için e-Arşiv formatı:**
   - Alıcı VKN: şirket için `2222222222`, kişi için `11111111111`.
   - İstisna kodu **302** (11/1-a Hizmet İhracı), KDV %0.
   - Döviz cinsi ve kur.
   - Açıklama/not satırı. Önerilen metinler:
     - AB şirketleri: "Reverse charge – VAT to be accounted for by the recipient, Art. 196 Council
       Directive 2006/112/EC";
     - İngiltere şirketleri: "Reverse charge – the customer accounts for UK VAT (VAT Act 1994,
       section 8)";
     - tüm yurt dışı alıcılar: "Hizmet ihracı – KDV Kanunu md. 11/1-a uyarınca KDV'den istisnadır".
   - Ayrıca faturada alıcının adı veya unvanı, ülkesi ve varsa KDV numarası yer almalı mı?

   Not: saas_be'deki Turkcell e-Arşiv entegrasyonu Pigeonpost için şu an **test modunda**; canlı
   fatura kesmiyor. Canlıya almadan önce bu kod ve metinlerin eklenmesi gerekiyor. O zamana kadar
   faturalar sizin tarafınızda düzenlenmeli.
