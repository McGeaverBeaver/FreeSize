<?php

declare(strict_types=1);

// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The app's checks that need no Nextcloud: `php tests/run.php`. The vectors
// come from the Rust side (`freesize_proto`), so both sign the same way.

require __DIR__ . '/../lib/Hub/Keys.php';
require __DIR__ . '/../lib/Hub/Link.php';
require __DIR__ . '/../lib/Service/Deleter.php';
require __DIR__ . '/../lib/Service/Updater.php';

use OCA\FreeSize\Hub\Keys;
use OCA\FreeSize\Hub\Link;
use OCA\FreeSize\Service\Deleter;
use OCA\FreeSize\Service\Updater;

$failed = 0;
function check(string $name, mixed $got, mixed $want): void {
	global $failed;
	if ($got !== $want) {
		$failed++;
		echo "FAIL $name: got " . var_export($got, true) . ', want ' . var_export($want, true) . "\n";
	} else {
		echo "ok   $name\n";
	}
}

$one = str_repeat("\x01", 32);
$seven = str_repeat("\x07", 32);
check('public key', base64_encode(Keys::publicKey($one)), 'iojj3XQJ8ZX9UtstPLpdcspnCb8dlBIb83SIAbQPb1w=');
check('fingerprint', Keys::fingerprint(Keys::publicKey($one)), '6HTG-Z65X-B7YF-S53D');
check('signature', Keys::sign($seven, 'hello'), 'NZoxWSDZVBw8wqHdGDnz5AvyM1ih2TpuvYMDwDEM61Al5nkiKrAWtNgixQAeeH4AwM6qasPG6AJIqUS9RxBPDA==');
check('crockford', Keys::crockford(hex2bin('deadbeef010203')), 'VTPVXVR1081G');

$sealed = Keys::seal(['command' => 'x', 'allow' => true], $seven, 'decision');
check('envelope opens', Keys::open($sealed, Keys::publicKey($seven), 'decision'), ['command' => 'x', 'allow' => true]);
check('envelope for another purpose', Keys::open($sealed, Keys::publicKey($seven), 'command'), null);
check('envelope from another key', Keys::open($sealed, Keys::publicKey($one), 'decision'), null);
$forged = $sealed;
$forged['body'] = '{"command":"x","allow":false}';
check('envelope changed', Keys::open($forged, Keys::publicKey($seven), 'decision'), null);

check('code formatted', Keys::formatCode('7kq2 m9xd 4hrt'), '7KQ2-M9XD-4HRT');
check('code look-alikes', Keys::formatCode('OIL0-0000-0000'), '0110-0000-0000');
check('not a code', Keys::formatCode('1234'), null);

check('link', Link::parse('freesize://pair?hub=https%3A%2F%2Fhub.lan%3A9090&code=7KQ2M9XD4HRT&key=abcd-efgh-jkmn-pqrs'),
	['hub' => 'https://hub.lan:9090', 'code' => '7KQ2-M9XD-4HRT', 'key' => 'ABCD-EFGH-JKMN-PQRS']);
check('address and code', Link::parse('hub.lan:9090/', '7KQ2-M9XD-4HRT'), ['hub' => 'https://hub.lan:9090', 'code' => '7KQ2-M9XD-4HRT', 'key' => null]);
check('plain http kept', Link::normalizeHub('http://hub.lan'), 'http://hub.lan');

check('path', Deleter::clean('/Photos//2019/a.jpg'), 'Photos/2019/a.jpg');
check('path up', Deleter::clean('Photos/../../admin/files'), null);
check('whole folder', Deleter::clean('/'), null);
check('empty', Deleter::clean(''), null);
check('backslash', Deleter::clean('a\\..\\b'), null);

$headers = Keys::requestHeaders($one, 'DEV1', 'post', '/api/v1/poll', '{}', 1700000000);
$message = "disktree-request-v1\nPOST\n/api/v1/poll\n1700000000\n{$headers['x-disktree-nonce']}\n" . hash('sha256', '{}');
check('request signature', Keys::verify(Keys::publicKey($one), $message, $headers['x-disktree-signature']), true);

echo $failed === 0 ? "all passed\n" : "$failed failed\n";

check('version words', Updater::versionNumber('FreeSize Nextcloud app 0.20.2'), [0, 20, 2]);
check('version tag', Updater::versionNumber('v0.21'), [0, 21, 0]);
check('not a version', Updater::versionNumber('latest'), null);
check('newer', Updater::newer('0.21.0', 'FreeSize Nextcloud app 0.20.2'), true);
check('not newer', Updater::newer('0.20.2', '0.20.10'), false);
check('key line', Updater::keyIn("# comment\n\niojj3XQJ8ZX9UtstPLpdcspnCb8dlBIb83SIAbQPb1w=\n"), base64_decode('iojj3XQJ8ZX9UtstPLpdcspnCb8dlBIb83SIAbQPb1w='));
check('no key', Updater::keyIn("# only comments\n"), null);
$releases = [
	['tag_name' => 'v0.22.0', 'draft' => true, 'assets' => [['name' => 'freesize-nextcloud-0.22.0.tar.gz', 'browser_download_url' => 'd']]],
	['tag_name' => 'v0.21.1', 'prerelease' => true, 'assets' => [['name' => 'freesize-nextcloud-0.21.1.tar.gz', 'browser_download_url' => 'p']]],
	['tag_name' => 'v0.21.0', 'html_url' => 'page', 'assets' => [
		['name' => 'freesize-nextcloud-0.21.0.tar.gz', 'browser_download_url' => 'tar'],
		['name' => 'SHA256SUMS.txt', 'browser_download_url' => 'sums'],
		['name' => 'SHA256SUMS.txt.sig', 'browser_download_url' => 'sig'],
	]],
	['tag_name' => 'v0.20.9', 'assets' => []],
	['tag_name' => 'v0.19.0', 'assets' => [['name' => 'freesize-nextcloud-0.19.0.tar.gz', 'browser_download_url' => 'old']]],
];
check('picks the newest published release with the tarball', Updater::pick($releases, '0.20.2'),
	['version' => '0.21.0', 'url' => 'page', 'asset' => 'freesize-nextcloud-0.21.0.tar.gz', 'download' => 'tar', 'sums' => 'sums', 'signature' => 'sig']);
check('nothing newer', Updater::pick($releases, '0.21.0'), null);
check('sum for the tarball', Updater::sumFor("abc\n" . str_repeat('a', 64) . "  freesize-hub.tar.gz\n" . str_repeat('B', 64) . " *freesize-nextcloud-0.21.0.tar.gz\n", 'freesize-nextcloud-0.21.0.tar.gz'), str_repeat('b', 64));
check('no sum', Updater::sumFor('', 'x'), null);
check('entry', Updater::safeEntry('freesize/lib/Service/Updater.php'), true);
check('entry folder', Updater::safeEntry('freesize/lib/'), true);
check('entry elsewhere', Updater::safeEntry('other/file.php'), false);
check('entry up', Updater::safeEntry('freesize/../config/config.php'), false);
check('entry absolute', Updater::safeEntry('/freesize/x'), false);

exit($failed === 0 ? 0 : 1);
