package db

import (
	"database/sql"
	"errors"

	"github.com/Spelunkern/Rapture/internal/config"
)

// FactionNone is reported while the account has not chosen a faction yet.
const FactionNone = 2

// ReadMaxGrow runs PS_GameData.dbo.usp_Read_User_MaxGrow_R. The original
// procedure always reports MaxGrow 3; without a row the account has no
// faction yet.
func ReadMaxGrow(userUID int32) (country, maxGrow uint8, err error) {
	pool, err := Pool(GameData)
	if err != nil {
		return 0, 0, err
	}
	c, cancel := ctx()
	defer cancel()
	var countryValue int16
	var maxGrowValue int32
	err = pool.QueryRowContext(c, "dbo.usp_Read_User_MaxGrow_R",
		sql.Named("ServerID", config.ServerID), sql.Named("UserUID", userUID)).Scan(&countryValue, &maxGrowValue)
	if errors.Is(err, sql.ErrNoRows) {
		return FactionNone, 3, nil
	}
	if err != nil {
		return 0, 0, err
	}
	return uint8(countryValue), uint8(maxGrowValue), nil
}

// SaveCountry runs PS_GameData.dbo.usp_Save_Char_Country_E, inserting the
// UserMaxGrow row the first time an account picks a faction.
func SaveCountry(userUID int32, country uint8) error {
	current, _, err := ReadMaxGrow(userUID)
	if err != nil {
		return err
	}
	pool, err := Pool(GameData)
	if err != nil {
		return err
	}
	c, cancel := ctx()
	defer cancel()
	_, err = pool.ExecContext(c, "dbo.usp_Save_Char_Country_E",
		sql.Named("ServerID", config.ServerID), sql.Named("UserUID", userUID),
		sql.Named("Country", int16(country)), sql.Named("Insert", current == FactionNone))
	return err
}
