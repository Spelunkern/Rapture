package db

import (
	"database/sql"
	"errors"
	"fmt"
)

// LoginResult is what PS_UserData.dbo.usp_Try_GameLogin_Taiwan selects.
type LoginResult struct {
	Status  int32
	UserUID int32
}

// TryGameLogin runs the original login procedure. It also writes the login
// log and UserLoginStatus rows exactly as the original server did.
func TryGameLogin(userID, password string, sessionID int64, userIP string) (LoginResult, error) {
	pool, err := Pool(UserData)
	if err != nil {
		return LoginResult{}, err
	}
	c, cancel := ctx()
	defer cancel()
	var result LoginResult
	err = pool.QueryRowContext(c, "dbo.usp_Try_GameLogin_Taiwan",
		sql.Named("UserID", userID),
		sql.Named("InPassword", password),
		sql.Named("SessionID", sessionID),
		sql.Named("UserIP", userIP),
	).Scan(&result.Status, &result.UserUID)
	if err != nil {
		return LoginResult{}, fmt.Errorf("usp_Try_GameLogin_Taiwan: %w", err)
	}
	return result, nil
}

// TryGameLogout runs the original logout procedure.
func TryGameLogout(userUID int32, sessionID int64) error {
	pool, err := Pool(UserData)
	if err != nil {
		return err
	}
	c, cancel := ctx()
	defer cancel()
	_, err = pool.ExecContext(c, "dbo.usp_Try_GameLogout_R",
		sql.Named("UserUID", userUID), sql.Named("SessionID", sessionID))
	return err
}

// LoginDecision maps the procedure's Status to the client's login result
// byte and permission (auth status), as recovered from the original
// ps_login (CUser login handling; see Eternity src/auth/login_protocol.cpp).
// Permission 11 is a regular account; 1..5 are the admin tiers.
func LoginDecision(login LoginResult) (result uint8, permission uint8) {
	permission = 11
	if login.UserUID == 0 {
		return 1, permission
	}
	switch login.Status {
	case -3:
		result = 1
	case -2:
		result = 2
	case -1:
		result = 3
	case 1:
		result = 9
	case 2:
		result = 4
	case 6:
		result = 5
	case 0x11:
		result = 10
	case 0x21:
		result = 11
	case 0x31:
		result = 12
	case 0x41:
		result = 13
	case 0x51:
		result = 14
	case 0x80:
		result = 6
	case 0x100:
		result = 7
	case 0x200:
		result = 8
	default:
		switch {
		case login.Status&2 != 0:
			result = 10
		case login.Status&1 != 0:
			result = 11
		case (uint32(login.Status)>>7)&7 != 0:
			result = 6
		default:
			permission = uint8((login.Status >> 4) & 7)
			if permission == 0 {
				permission = 11
			}
		}
	}
	return result, permission
}

// Account is the account state the world server needs.
type Account struct {
	UserUID    int32
	UserID     string
	Status     int16
	Admin      bool
	AdminLevel uint8
	Point      int32
	Country    uint8 // 0 light, 1 dark, 2 none chosen yet
	MaxGrow    uint8
}

var ErrNotFound = errors.New("not found")

// AccountByUID reads Users_Master plus the realm's UserMaxGrow row
// (PS_GameData.dbo.usp_Read_User_MaxGrow_R).
func AccountByUID(userUID int32) (Account, error) {
	pool, err := Pool(UserData)
	if err != nil {
		return Account{}, err
	}
	c, cancel := ctx()
	defer cancel()
	var a Account
	err = pool.QueryRowContext(c, `SELECT UserUID, UserID, Status, Admin, AdminLevel, Point
		FROM dbo.Users_Master WHERE UserUID = @p1 AND Leave = 0`, userUID).
		Scan(&a.UserUID, &a.UserID, &a.Status, &a.Admin, &a.AdminLevel, &a.Point)
	if errors.Is(err, sql.ErrNoRows) {
		return Account{}, ErrNotFound
	}
	if err != nil {
		return Account{}, err
	}
	a.Country, a.MaxGrow, err = ReadMaxGrow(userUID)
	return a, err
}
