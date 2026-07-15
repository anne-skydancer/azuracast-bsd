<?php

declare(strict_types=1);

namespace App\Entity\Migration;

use Doctrine\DBAL\Schema\Schema;

final class Version20260715120000 extends AbstractMigration
{
    public function getDescription(): string
    {
        return 'Remove "enable_liquidsoap_editing" from settings (Liquidsoap backend removed).';
    }

    public function up(Schema $schema): void
    {
        $this->addSql('ALTER TABLE settings DROP enable_liquidsoap_editing');
    }

    public function down(Schema $schema): void
    {
        $this->addSql('ALTER TABLE settings ADD enable_liquidsoap_editing TINYINT NOT NULL AFTER api_access_control');
    }
}
