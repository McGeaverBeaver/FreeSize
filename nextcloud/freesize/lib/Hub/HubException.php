<?php

declare(strict_types=1);

// SPDX-License-Identifier: AGPL-3.0-or-later

namespace OCA\FreeSize\Hub;

class HubException extends \RuntimeException {
	public function __construct(
		string $message,
		/** The Hub's HTTP status; 0 when it wasn't reached. */
		public readonly int $status = 0,
		/** The Hub's fingerprint, when the admin has to compare it. */
		public readonly ?string $fingerprint = null,
		/** The Hub removed this device: forget the pairing. */
		public readonly bool $removed = false,
	) {
		parent::__construct($message);
	}

	/** Worth trying again later. */
	public function temporary(): bool {
		return $this->status === 0 || $this->status >= 500;
	}
}
