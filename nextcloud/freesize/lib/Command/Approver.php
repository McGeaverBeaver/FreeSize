<?php

declare(strict_types=1);

// SPDX-License-Identifier: AGPL-3.0-or-later

namespace OCA\FreeSize\Command;

use OCA\FreeSize\Hub\HubException;
use OCA\FreeSize\Service\Agent;
use Symfony\Component\Console\Command\Command;
use Symfony\Component\Console\Input\InputArgument;
use Symfony\Component\Console\Input\InputInterface;
use Symfony\Component\Console\Input\InputOption;
use Symfony\Component\Console\Output\OutputInterface;

class Approver extends Command {
	public function __construct(
		private Agent $agent,
	) {
		parent::__construct();
	}

	protected function configure(): void {
		$this->setName('freesize:approver')
			->setDescription('Show or choose the phone that approves deletes (its fingerprint is under Settings, Hub on the phone)')
			->addArgument('fingerprint', InputArgument::OPTIONAL, 'The approver\'s fingerprint')
			->addOption('clear', null, InputOption::VALUE_NONE, 'Remove the approver: every request is refused')
			->addOption('end-grants', null, InputOption::VALUE_NONE, 'End the grants the approver gave');
	}

	protected function execute(InputInterface $input, OutputInterface $output): int {
		$state = $this->agent->state();
		if (!$state->paired()) {
			$output->writeln('<error>Pair with a Hub first: occ freesize:pair LINK</error>');
			return 1;
		}
		try {
			if ($input->getOption('end-grants')) {
				$output->writeln('Ended ' . $this->agent->endGrants() . ' grant(s).');
			}
			if ($input->getOption('clear')) {
				$this->agent->setApprover(null);
				$output->writeln('No approver: requests from the Hub are refused.');
				return 0;
			}
			$fingerprint = $input->getArgument('fingerprint');
			if ($fingerprint !== null) {
				$name = $this->agent->setApprover($fingerprint);
				$output->writeln("Requests are approved by $name now.");
				return 0;
			}
		} catch (HubException $e) {
			$output->writeln('<error>' . $e->getMessage() . '</error>');
			return 1;
		}
		$approver = $state->approver();
		$output->writeln($approver === null
			? 'No approver: requests from the Hub are refused.'
			: "Requests are approved by {$approver['name']} ({$state->approverFingerprint()}).");
		return 0;
	}
}
