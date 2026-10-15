<?php

declare(strict_types=1);

// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The app's checks that need no Nextcloud: `php tests/run.php`. The vectors
// come from the Rust side (`freesize_proto`), so both sign the same way.

require __DIR__ . '/../lib/Hub/Keys.php';
require __DIR__ . '/../lib/Hub/Link.php';
require __DIR__ . '/../lib/Service/Deleter.php';

use OCA\FreeSize\Hub\Keys;
use OCA\FreeSize\Hub\Link;
use OCA\FreeSize\Service\Deleter;

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
exit($failed === 0 ? 0 : 1);
