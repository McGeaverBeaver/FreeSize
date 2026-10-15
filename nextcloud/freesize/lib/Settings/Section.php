<?php

declare(strict_types=1);

// SPDX-License-Identifier: AGPL-3.0-or-later

namespace OCA\FreeSize\Settings;

use OCP\IURLGenerator;
use OCP\Settings\IIconSection;

class Section implements IIconSection {
	public function __construct(
		private IURLGenerator $url,
	) {
	}

	public function getID(): string {
		return 'freesize';
	}

	public function getName(): string {
		return 'FreeSize';
	}

	public function getPriority(): int {
		return 80;
	}

	public function getIcon(): string {
		return $this->url->imagePath('freesize', 'app-dark.svg');
	}
}
