> **Draft — not in force — needs legal review.**
> Prepared for the owner's lawyer and accountant. Do not add these paragraphs to any live page
> until counsel approves them. Text in square brackets is a placeholder to be decided.

# Privacy policy additions for the Nests marketplace

These are the paragraphs to add to the two privacy pages when the marketplace opens for paid
sales. They follow the style of the existing Nests sections.

## 1. `site/privacy.html`

Add after the **Nests (hosted AI agents)** section (`<h3 id="nests">`), before the next heading:

```html
<h3 id="nest-apps">Nest apps from other publishers</h3>
<p>
  You can install apps made by other companies or people (publishers) into your Nests, from the
  Nests marketplace. A Nest app runs inside your Nest's isolated sandbox on our Nests server. It
  can use the data in that Nest, as its listing describes. Like the rest of a Nest, it is not
  end-to-end encrypted: to run, the app and our servers read the data it works on.
</p>
<p>
  <strong>Who is responsible.</strong> You decide what data goes into your Nest and what the app
  does with it, so you are the controller of that data. We process it for you as your processor.
  The publisher processes it through the app as our sub-processor, under a written agreement that
  binds it to our instructions, to confidentiality and to deletion. Marketplace sales on the web
  are made by Piyote Bilişim ve Danışmanlık Hizmetleri Ticaret Ltd. Şti., Türkiye, which processes
  your order and billing details as controller for that purpose.
</p>
<p>
  <strong>Review.</strong> We build every release from its source code, scan it, review it with
  AI models that run on servers we operate, test its behaviour and have a member of our staff
  approve it. No outside AI provider receives the source or your data for this.
</p>
<p>
  <strong>Where the data can go.</strong> An app has no direct network access. It reaches the
  outside only through our broker, and only to the destinations shown on its listing. It cannot
  act outside Pigeonpost unless you approve that exact action first.
</p>
<p>
  <strong>What publishers receive from us.</strong> Only redacted operation records about their
  app: for example counts, status, error codes, timings and release versions. We do not give
  publishers your messages, files, names, handles or contact addresses. A publisher has no access
  to your Nest. Data reaches a publisher only if the app sends it to a destination listed on its
  listing, as part of what the app does.
</p>
<p>
  <strong>Payments.</strong> iyzico processes card payments. We and Piyote receive the payment
  status, the card's last digits and the order details, never the full card number.
</p>
<p>
  <strong>Retention and deletion.</strong> When you uninstall an app, its data in that
  installation is deleted [at once / within [period]]. If a monthly app is not paid, it is paused
  at the due time and its data in that installation is deleted 72 hours later. This never affects
  your Inboxes, handles, organisations or Nest subscriptions. Deleted app data stays in our
  encrypted backups until they expire, as for Nests, and is not brought back. Order and invoice
  records are kept for [legal retention period] because tax law requires it.
</p>
<p>
  <strong>Transfers.</strong> Nest apps run on our servers in [country]. If an app's listing names
  a destination outside Türkiye, using that app transfers the data sent there. [Counsel: transfer
  basis under KVKK Article 9 and, where it applies, the GDPR.]
</p>
```

Add these rows to the retention table, after the Nests row:

```html
<tr>
  <td>Nest app data inside an installation</td>
  <td>To run the apps you install in your Nests</td>
  <td>Processed on your behalf, as your processor (GDPR Art. 28; KVKK Art. 12)</td>
  <td>Until you uninstall the app or delete the Nest; <strong>72 hours</strong> after a missed monthly payment; encrypted backups as for Nests</td>
</tr>
<tr>
  <td>Marketplace orders, invoices and refunds</td>
  <td>To sell, invoice and refund Nest apps, and pay publishers</td>
  <td>Performance of the contract (GDPR Art. 6(1)(b)); legal obligation for tax and commercial records (Art. 6(1)(c))</td>
  <td>[legal retention period]</td>
</tr>
```

The table's columns are: data, purpose, legal basis, retention.

## 2. `site/app-privacy.html`

Add one list item after the **Nests (hosted AI agents)** item:

```html
<li><strong>Nest apps from other publishers:</strong> apps you install into a Nest from the Nests marketplace run inside that Nest's sandbox and can use its data, as their listing describes. You are the controller of that data; we process it for you, and the publisher processes it through the app as our sub-processor. Apps reach the outside only through our broker, to the destinations on their listing, and cannot act outside Pigeonpost without your approval of that exact action. Publishers receive only redacted operation records, never your messages, files, names or contact addresses. Nest apps are bought on the web from Piyote Bilişim ve Danışmanlık Hizmetleri Ticaret Ltd. Şti.; this app only uses apps you already own. See our <a href="https://pigeonpost.dev/privacy.html#nest-apps">privacy policy</a>.</li>
```

Add this sentence to the retention paragraph, after the sentence about Nests:

```html
Nest app data is deleted when you uninstall the app, or 72 hours after a missed monthly payment for that app.
```

## Özet (Türkçe)

> Taslaktır — yürürlükte değildir — hukuki inceleme gerekir.

- `site/privacy.html` sayfasına Nests bölümünden sonra "Nest apps from other publishers" başlıklı
  yeni bir bölüm ve saklama tablosuna iki satır eklenecektir. `site/app-privacy.html` sayfasına bir
  madde ve bir cümle eklenecektir.
- **Roller.** Alıcı veri sorumlusu; Pigeonpost veri işleyen; yayıncı alt işleyen (yazılı sözleşmeyle).
  Web satışlarında sipariş ve fatura verisi için Piyote veri sorumlusudur.
- **Yayıncıya paylaşılanlar.** Yalnızca maskelenmiş işlem kayıtları (sayılar, durum, hata kodları,
  süreler, sürüm). Mesaj, dosya, ad, kullanıcı adı veya iletişim adresi paylaşılmaz.
- **Dış ağ.** Yalnızca Pigeonpost aracısı üzerinden, ilanda gösterilen adreslere; dış işlemler alıcı
  onayıyla.
- **Saklama ve silme.** Kaldırıldığında silinir; aylık ödeme yapılmazsa 72 saat sonra silinir;
  yedeklerde süresi dolana kadar kalır, geri getirilmez. Fatura kayıtları yasal süre boyunca saklanır.
- **Yurt dışı aktarım** (KVKK m. 9) avukat tarafından değerlendirilmelidir.
